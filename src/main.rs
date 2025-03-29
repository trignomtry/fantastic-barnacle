use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, get, post, web};
use actix_ws::{AggregatedMessage, Session};
use futures_util::stream::StreamExt;
use rand::{Rng as _, distr::Alphanumeric};
use rustrict::CensorStr;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::Mutex, time::sleep};

struct State {
    games: HashMap<String, Arc<Mutex<Game>>>,
    activities: HashMap<String, Activity>,
}

impl State {
    fn new(activities: HashMap<String, Activity>) -> State {
        State {
            games: HashMap::new(),
            activities,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Activity {
    name: String,
    config: Config,
    access: Access,
}

#[derive(Serialize, Deserialize, Clone, Debug)]

enum Access {
    All(String),
    Usernames(Vec<String>),
}

struct Game {
    players: Vec<Arc<Mutex<Player>>>,
    phase: GamePhase,
    teacher_session: Arc<Mutex<Session>>,
    config: Config,
}

enum GamePhase {
    Lobby(),
    OnQuestion(u64),
}

#[derive(Clone)]
struct Player {
    name: String,
    socket: Arc<Mutex<Session>>,
    points: u64,
    curr_answer: Option<usize>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
enum Config {
    Slides(Vec<SlidesItem>),
    Quiz { questions: Vec<Question> },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
enum SlidesItem {
    Slide(),
    Question(Question),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Question {
    text: String,
    responses: Vec<Response>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Response {
    text: String,
    correct: bool,
}

fn ugg(err: &str) -> HttpResponse {
    HttpResponse::Ok().json(json!({"error": err}))
}

#[get("/api/ws/{code}/{name}")]
async fn join_game(
    req: HttpRequest,
    stream: web::Payload,
    path: web::Path<(String, String)>,
    state: web::Data<Arc<Mutex<State>>>,
) -> Result<HttpResponse, actix_web::Error> {
    let (res, session, stream) = actix_ws::handle(&req, stream)?;
    let session = Arc::new(Mutex::new(session));
    let (code, name) = path.into_inner();
    let game = {
        let binding = state.lock().await;
        match binding.games.get(&code) {
            Some(game_arc) => game_arc.clone(),
            None => return Err(actix_web::error::ErrorNotFound("Game not found")),
        }
    };
    if name.is_inappropriate() {
        return Err(actix_web::error::ErrorForbidden("Name is Inappropriate"));
    }

    let session_clone = session.clone();
    let name_clone = name.clone();
    let player = Arc::new(Mutex::new(Player {
        name: name_clone,
        socket: session,
        points: 0,
        curr_answer: None,
    }));
    game.lock().await.players.push(player.clone());

    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));
    actix_web::rt::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(AggregatedMessage::Text(text)) => {
                    let mut session = session_clone.lock().await;
                    match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(val) => match val["event"].as_str() {
                            Some("answer") => {
                                if let Some(answer) = val["answer"].as_u64() {
                                    if let GamePhase::OnQuestion(_) = game.lock().await.phase {
                                        player.lock().await.curr_answer = Some(answer as usize);
                                    }
                                };
                            }
                            Some("joined") => {
                                session.text("joinconfirmed").await.unwrap();
                                match game
                                    .lock()
                                    .await
                                    .teacher_session
                                    .lock()
                                    .await
                                    .text(json!({ "event": "playerjoined", "player": { "name": name }}).to_string())
                                    .await
                                {
                                    Ok(_) => {}
                                    Err(_) => {
                                        for i in 0..game.lock().await.players.len() {
                                            match game.lock().await.players[i].clone()
                                                .lock().await
                                                .socket
                                                .clone()
                                                .lock()
                                                .await
                                                .text("teacherdisconnect")
                                                .await
                                            {
                                                Ok(_) => {}
                                                Err(_) => {
                                                    game.lock().await.players.remove(i);
                                                }
                                            };
                                        }
                                    }
                                };
                            }
                            _ => {
                                let _ = session.text("{ \"error\": \"Command not found\" }").await;
                            }
                        },
                        Err(_) => {
                            let _ = session.text("{ \"error\": \"Command not found\" }").await;
                        }
                    };
                }
                Ok(AggregatedMessage::Binary(bin)) => {
                    let mut session = session_clone.lock().await;
                    session.binary(bin).await.unwrap();
                }
                Ok(AggregatedMessage::Ping(msg)) => {
                    let mut session = session_clone.lock().await;
                    session.pong(&msg).await.unwrap();
                }
                _ => {}
            }
        }
    });

    Ok(res)
}

#[get("/api/teacher/{app_key}/start_game/{game_id}")]
async fn start_game(
    req: HttpRequest,
    stream: web::Payload,
    state: web::Data<Arc<Mutex<State>>>,
    path: web::Path<(String, String)>,
) -> Result<HttpResponse, actix_web::Error> {
    let (res, session, stream) = actix_ws::handle(&req, stream)?;
    let session = Arc::new(Mutex::new(session));

    let (app_key, game_id) = path.into_inner();

    let session_clone = session.clone();
    let code = rand::rng().random_range(1000..=9999).to_string();
    let activity = {
        let binding = state.lock().await;
        match binding.activities.get(&game_id) {
            Some(activity) => activity.clone(),
            None => return Err(actix_web::error::ErrorNotFound("Set not found")),
        }
    };
    let access: bool = match activity.access.clone() {
        Access::All(_) => true,
        Access::Usernames(v) => {
            let mut vvvvvv = false;
            let teacheru = reqwest::Client::new()
                .get(format!(
                    "https://www.vortice.app/api/apps/{}/userdata",
                    app_key
                ))
                .send()
                .await;
            if let Ok(e) = teacheru {
                if let Ok(e) = e.text().await {
                    if let Ok(ee) = serde_json::from_str::<Value>(&e) {
                        if let Some(r) = ee["username"].as_str() {
                            if v.contains(&r.to_string()) {
                                vvvvvv = true;
                            }
                        }
                    }
                }
            }
            vvvvvv
        }
    };
    if !access {
        return Err(actix_web::error::ErrorForbidden(
            "You do not have access to this set",
        ));
    }
    let state_clone = state.clone();
    let mut binding = state_clone.lock().await;
    let g = Arc::new(Mutex::new(Game {
        config: activity.config,
        phase: GamePhase::Lobby(),
        players: vec![],
        teacher_session: session.clone(),
    }));
    binding.games.insert(code.clone(), g.clone());

    match session
        .lock()
        .await
        .text(
            json!(
                { "event": "gamecode",  "gamecode": code }
            )
            .to_string(),
        )
        .await
    {
        Ok(_) => {}
        Err(_) => {
            let mut binding = state_clone.lock().await;
            binding.games.remove(&code);
        }
    };

    let state_for_spawn = state.clone();
    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));
    actix_web::rt::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(AggregatedMessage::Text(text)) => {
                    let mut session = session_clone.lock().await;
                    if text == "gamestart" {
                        let mut state_guard = state_for_spawn.lock().await;
                        let game = match state_guard.games.get_mut(&code) {
                            Some(r) => r,
                            None => {
                                session.text(json!({ "event": "error", "error": "Game not found on server" }).to_string()).await.unwrap();
                                panic!();
                            }
                        };
                        let binding = game.clone();
                        let mut game = binding.lock().await;
                        game.phase = GamePhase::OnQuestion(0);
                        for i in 0..game.players.len() {
                            let player = &game.players[i].clone();
                            if player
                                .lock()
                                .await
                                .socket
                                .lock()
                                .await
                                .text("gamestart")
                                .await
                                .is_err()
                            {
                                game.players.remove(i);
                            }
                            if let Config::Quiz { questions } = &game.config {
                                player
                                    .lock().await.socket
                                    .lock()
                                    .await
                                    .text(json!({"event": "new_question", "question": {"number": 0, "text": questions[0].text, "responses": questions[0].responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>() }}).to_string())
                                    .await.unwrap();
                            }
                        }
                        match session
                            .text(json!({ "event": "gamestart" }).to_string())
                            .await
                        {
                            Ok(_) => {}
                            Err(_) => {
                                for i in 0..game.players.len() {
                                    let bindign = game.players[i].clone();
                                    let player = bindign.lock().await;
                                    if player.socket.lock().await.text("gameend").await.is_err() {
                                        game.players.remove(i);
                                    }
                                }
                            }
                        };
                        if let Config::Quiz { questions } = &game.config {
                            session
                                .text(json!({"event": "new_question", "question": {"text":questions[0].text, "responses": questions[0].responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>() }}).to_string())
                                .await.unwrap();
                        }
                    } else if text == "continue" {
                        let mut state_guard = state_for_spawn.lock().await;
                        let game = match state_guard.games.get_mut(&code) {
                            Some(g) => g,
                            None => {
                                session
                                    .text(json!({ "event": "error", "error": "Game not found on server" }).to_string())
                                    .await
                                    .unwrap();
                                continue;
                            }
                        };
                        let binding = game.clone();
                        let mut game = binding.lock().await;
                        for player in game.players.clone() {
                            let mut player = player.lock().await;
                            if let Some(curr_answer) = player.curr_answer {
                                if let GamePhase::OnQuestion(qnum) = game.phase {
                                    if let Config::Quiz { questions } = &game.config {
                                        if questions[qnum as usize].responses[curr_answer].correct {
                                            player.points += 600;
                                            player.curr_answer = None;
                                        }
                                    }
                                }
                            };
                        }

                        let leaderboard = game.players.clone();
                        let mut leaderboard_data = Vec::new();
                        for player in &leaderboard {
                            let guard = player.lock().await;
                            leaderboard_data.push((guard.name.clone(), guard.points));
                        }

                        // Sort the vector synchronously by points
                        leaderboard_data.sort_by(|a, b| b.1.cmp(&a.1));

                        let leaderboard_json: Vec<_> = leaderboard_data
                            .iter()
                            .map(|(name, points)| json!({ "name": name, "points": points }))
                            .collect();

                        session
                            .text(
                                json!({
                                    "event": "leaderboard",
                                    "leaderboard": leaderboard_json
                                })
                                .to_string(),
                            )
                            .await
                            .unwrap();
                        if let GamePhase::OnQuestion(qnum) = game.phase {
                            sleep(Duration::from_secs(5)).await;
                            game.phase = GamePhase::OnQuestion(qnum + 1);
                            if let Config::Quiz { questions } = &game.config.clone() {
                                for i in 0..game.players.len() {
                                    let playa = &game.players[i];
                                    let next_question = questions[(qnum + 1) as usize].clone();
                                    if playa.lock().await.socket.lock().await.text(json!({"event": "new_question", "question": {"text": next_question.text, "responses": next_question.responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>()}}).to_string()).await.is_err() {
                                        game.players.remove(i);
                                    }
                                }
                                if session.text(json!({"event": "new_question", "question": {"text": questions[(qnum + 1) as usize].text, "responses": questions[(qnum + 1) as usize].responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>()}}).to_string()).await.is_err(){
                                    for i in 0..game.players.len() {
                                        let playa = &game.players[i];
                                        if playa.lock().await.socket.lock().await.text(json!({"event": "gameend" }).to_string()).await.is_err() {
                                            game.players.remove(i);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(AggregatedMessage::Binary(bin)) => {
                    let mut session = session_clone.lock().await;
                    session.binary(bin).await.unwrap();
                }
                Ok(AggregatedMessage::Ping(msg)) => {
                    let mut session = session_clone.lock().await;
                    session.pong(&msg).await.unwrap();
                }
                _ => {}
            }
        }
    });

    Ok(res)
}

#[derive(Serialize, Deserialize)]
struct CreateActivity {
    name: String,
    access: Access,
    config: Config,
}

#[post("/api/teachers/{app_key}/create_activity")]
async fn create_activity(
    data: web::Json<CreateActivity>,
    path: web::Path<String>,
    state: web::Data<Arc<Mutex<State>>>,
) -> impl Responder {
    let data = data.into_inner();
    let app_key = path.into_inner();
    let respo = reqwest::Client::new()
        .post(format!("https://www.vortice.app/api/apps/{}", app_key))
        .body("read")
        .send()
        .await;

    if let Ok(r) = respo {
        if let Ok(rr) = r.text().await {
            if let Ok(rrr) = serde_json::from_str::<Value>(&rr) {
                let id: String = random(8);

                let mut id_list: Vec<Value> = vec![];

                if let Some(existing_data_str) = rrr["data"].as_str() {
                    if let Ok(parsed_data) = serde_json::from_str::<Value>(existing_data_str) {
                        if let Some(existing_ids) = parsed_data["data"].as_array() {
                            id_list = existing_ids.clone();
                        }
                    }
                }

                id_list.push(json!(id));

                let _ = reqwest::Client::new()
                    .post(format!("https://www.vortice.app/api/apps/{}", app_key))
                    .body(json!({ "data": id_list }).to_string())
                    .send()
                    .await;

                state.lock().await.activities.insert(
                    id.clone(),
                    Activity {
                        name: data.name,
                        access: data.access,
                        config: data.config,
                    },
                );
                let _ = std::fs::write(
                    Path::new("./activities.txt"),
                    serde_json::to_string(&state.lock().await.activities).unwrap(),
                );

                return HttpResponse::Ok().json(json!({ "status": "Success", "id": id }));
            }
        }
    }

    ugg("Error with Vortice services")
}

#[get("/api/{app_key}/get_sets")]
async fn get_sets(path: web::Path<String>, state: web::Data<Arc<Mutex<State>>>) -> impl Responder {
    println!("Get sets called");
    let mut arr: Vec<Value> = vec![];
    let app_key = path.into_inner();
    let vresponse = reqwest::Client::new()
        .post(format!("https://www.vortice.app/api/apps/{}", app_key))
        .body("read")
        .send()
        .await;
    if let Ok(a) = vresponse {
        if let Ok(aa) = a.text().await {
            if aa.is_empty() {
                return HttpResponse::Ok().json(json!([]));
            }
            if let Ok(aaaa) = serde_json::from_str::<Value>(aa.as_str()) {
                if let Some(r) = aaaa["data"].as_str() {
                    println!("raw vortice data: {}", r);
                    if let Ok(aaa) = serde_json::from_str::<Value>(r) {
                        if let Some(r) = aaa["data"].as_array() {
                            for val in r {
                                if let Some(l) = val.as_str() {
                                    if let Some(e) = state.lock().await.activities.get(l) {
                                        arr.push(
                                            json!({ "id": l ,"name": e.name, "config": e.config }),
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        return ugg("Response from Vortice Services was not formatted");
                    }
                }
            } else {
                return ugg("Response from Vortice was not formatted correctly");
            }
        } else {
            return ugg("Response from Vortice was not formatted");
        }
    } else {
        return ugg("No response from Vortice Services");
    }
    HttpResponse::Ok().json(arr)
}

#[get("/api/ws/get_error/{code}/{name}")]
async fn get_error(
    path: web::Path<(String, String)>,
    state: web::Data<Arc<Mutex<State>>>,
) -> impl Responder {
    let (name, code) = path.into_inner();
    if name.is_inappropriate() {
        return ugg("Name is inappropriate");
    }
    match state.lock().await.games.get(&code) {
        Some(_) => ugg("Unknown error, try again later"),
        None => ugg("Game not found, try again later"),
    }
}

fn random(n: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(n)
        .map(char::from)
        .collect()
}

#[actix_web::main]
async fn main() -> Result<(), std::io::Error> {
    println!("Starting server on http://localhost:8085");
    let exist = serde_json::from_str::<HashMap<String, Activity>>(
        std::fs::read_to_string(Path::new("./activities.txt"))
            .unwrap()
            .as_str(),
    )
    .unwrap();
    let state: Arc<Mutex<State>> = Arc::new(Mutex::new(State::new(exist)));
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .service(join_game)
            .service(start_game)
            .service(get_error)
            .service(get_sets)
            .service(create_activity)
            .service(actix_files::Files::new("/", "./static").index_file("index.html"))
    })
    .bind(("localhost", 8085))?
    .run()
    .await
}
