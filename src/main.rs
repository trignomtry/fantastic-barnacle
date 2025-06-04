use std::{collections::HashMap, path::Path, time::Duration};

use actix::{Actor, Addr, AsyncContext as _, Context, Handler, Message, MessageResult, WrapFuture};

use actix_web::{
    App, HttpRequest, HttpResponse, HttpServer, Responder, get, post,
    web::{self, Bytes},
};
use actix_ws::{AggregatedMessage, Session};
use futures_util::stream::StreamExt;
use rand::{Rng as _, distr::Alphanumeric};
use rustrict::CensorStr;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::sleep;

struct State {
    games: HashMap<String, Addr<Game>>,
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
    players: Vec<Addr<Player>>,
    phase: GamePhase,
    teacher: Addr<Teacher>,
    config: Config,
}

#[derive(Clone)]
enum GamePhase {
    Lobby,
    OnQuestion(u64),
}

impl Actor for Player {
    type Context = Context<Self>;
}
impl Actor for Teacher {
    type Context = Context<Self>;
}
impl Actor for State {
    type Context = Context<Self>;
}
impl Actor for Game {
    type Context = Context<Self>;
}
struct Player {
    name: String,
    socket: Session,
    points: u64,
    curr_answer: Option<u64>,
}

struct Teacher {
    socket: Session,
}

#[derive(Message)]
#[rtype(result = "StateResponse")]
enum StateCommand {
    StartGame(String),
    GetGame(String),
    GetActivity(String),
    CreateGame(String, Addr<Game>),
    DeleteGame(String),
    CreateActivity(String, Activity),
    SaveToFile,
}
enum StateResponse {
    Game(Option<Addr<Game>>),
    Activity(Option<Activity>),
    None,
}
impl Handler<StateCommand> for State {
    type Result = MessageResult<StateCommand>;
    fn handle(&mut self, msg: StateCommand, _: &mut Self::Context) -> Self::Result {
        match msg {
            StateCommand::StartGame(code) => {
                let games = self.games.clone();
                actix::spawn(async move {
                    if let Some(game) = games.get(&code) {
                            game.do_send(GameCommand::SetPhase(GamePhase::OnQuestion(0)));
                            game.do_send(GameCommand::SendTextToPlayers("gamestart".to_string()));
                            if let Ok(GameResponse::Config(conf)) =
                                &game.send(GameCommand::GetConfig).await
                            {
                                match conf {
                                    Config::Quiz { questions } => {
                                        game
                                        .do_send(GameCommand::SendTextToPlayers(json!({"event": "new_question", "question": {"number": 0, "text": questions[0].text, "responses": questions[0].responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>() }}).to_string()));
                                    }
                                    Config::Slides(slides) => {
                                        if let SlidesItem::Question(q) = &slides[0] {
                                            game
                                            .do_send(GameCommand::SendTextToPlayers(json!({"event": "new_question", "question": {"number": 0, "text": q.text, "responses": q.responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>() }}).to_string()));
                                        }
                                    }
                                }
                            }
                            game.do_send(GameCommand::StartGame);
                            if let Ok(GameResponse::Config(conf)) =
                                &game.send(GameCommand::GetConfig).await
                            {

                            }

                    }
                });

                MessageResult(StateResponse::None)
            }
            StateCommand::GetGame(name) => {
                let game = self.games.get(&name).cloned();
                MessageResult(StateResponse::Game(game))
            }
            StateCommand::GetActivity(name) => {
                let activity = self.activities.get(&name).cloned();
                MessageResult(StateResponse::Activity(activity))
            }
            StateCommand::CreateActivity(k, v) => {
                self.activities.insert(k, v);
                MessageResult(StateResponse::None)
            }
            StateCommand::CreateGame(key, value) => {
                self.games.insert(key, value);
                MessageResult(StateResponse::None)
            }
            StateCommand::DeleteGame(k) => {
                self.games.remove(&k);
                MessageResult(StateResponse::None)
            }
            StateCommand::SaveToFile => {
                let activities = self.activities.clone();
                let _ = std::fs::write(
                    Path::new("./activities.txt"),
                    match serde_json::to_string(&activities) {
                        Ok(a) => a,
                        Err(_) => "{}".to_string(),
                    },
                );
                MessageResult(StateResponse::None)
            }
        }
    }
}

#[derive(Message)]
#[rtype(result = "GameResponse")]
enum GameCommand {
    AddPlayer(Addr<Player>),
    TeacherDisconnect,
    GetPhase,
    Continue,
    GetConfig,
    SetPhase(GamePhase),
    SendTextToTeacher(String),
    SendTextToPlayers(String),
    StartGame,
    GenLeaderboard,
}

#[derive(Message)]
#[rtype(result = "()")]
enum GameResponse {
    Phase(GamePhase),
    Config(Config),
    None,
}

impl Handler<GameCommand> for Game {
    type Result = MessageResult<GameCommand>;
    fn handle(&mut self, msg: GameCommand, ctx: &mut Self::Context) -> Self::Result {
        match msg {
            GameCommand::AddPlayer(name) => {
                self.players.push(name);
            }
            GameCommand::SendTextToTeacher(text) => {
                self.teacher.do_send(TeacherCommand::SendText(text));
            }
            GameCommand::SendTextToPlayers(text) => {
                for pl in &self.players {
                    pl.do_send(PlayerCommand::SendText(text.clone()));
                }
            }
            GameCommand::SetPhase(num) => self.phase = num,
            GameCommand::TeacherDisconnect => {
                for pl in &self.players {
                    pl.do_send(PlayerCommand::SendText("teacherdisconnect".to_string()));
                }
            }
            GameCommand::GetPhase => return MessageResult(GameResponse::Phase(self.phase.clone())),
            GameCommand::GetConfig => {
                return MessageResult(GameResponse::Config(self.config.clone()));
            }
            GameCommand::GenLeaderboard => {
                let players = self.players.clone();
                let phase = self.phase.clone();
                let config = self.config.clone();
                let teacher = self.teacher.clone();
                actix::spawn(async move {
                    for player in players.clone() {
                        if let Ok(PlayerResponse::CurrentAnswer(Some(selection))) =
                            player.send(PlayerCommand::GetCurrAnswer).await
                        {
                            if let GamePhase::OnQuestion(n) = phase {
                                match config.clone() {
                                    Config::Quiz { questions } => {
                                        if questions[n as usize].responses[selection as usize]
                                            .correct
                                        {
                                            player.do_send(PlayerCommand::QuestionCorrect);
                                        }
                                    }
                                    Config::Slides(slides) => {
                                        if let SlidesItem::Question(q) = &slides[n as usize] {
                                            if q.responses[selection as usize].correct {
                                                player.do_send(PlayerCommand::QuestionCorrect);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let mut leaderboard_data = Vec::new();
                    for player in &players {
                        if let Ok(PlayerResponse::LeaderboardData(name, points)) =
                            player.send(PlayerCommand::GetLeaderboardData).await
                        {
                            leaderboard_data.push((name, points));
                        }
                    }

                    leaderboard_data.sort_by(|a, b| b.1.cmp(&a.1));
                    let rv = leaderboard_data
                        .iter()
                        .map(|r| {
                            let name = &r.0;
                            let p = r.1;
                            json!({"name": name, "points": p})
                        })
                        .collect::<Vec<Value>>();
                    let rv = json!({"event": "leaderboard", "leaderboard": rv}).to_string();

                    teacher.do_send(TeacherCommand::SendText(rv));
                });
            }
            GameCommand::Continue => {
                let config = self.config.clone();
                let players = self.players.clone();
                let phase = self.phase.clone();
                let teacher = self.teacher.clone();
                let myself = ctx.address();
                myself.do_send(GameCommand::GenLeaderboard);

                ctx.spawn(async move {
                    if let GamePhase::OnQuestion(qnum) = &phase {
                        sleep(Duration::from_secs(5)).await;


                        myself.do_send(
                            GameCommand::SetPhase(GamePhase::OnQuestion(qnum + 1)),
                        );
                        match &config {
                            Config::Quiz { questions } => {
                                let next_question = questions[(qnum + 1) as usize].clone();
                                for p in &players {
                                    p.do_send(PlayerCommand::SendText( json!({"event": "new_question", "question": {"text": next_question.text, "responses": next_question.responses.iter().map(|r| {json!({"text": r.text})}).collect::<Vec<Value>>()}}).to_string()));
                                }

                                teacher.do_send(TeacherCommand::SendText(json!({"event": "new_question", "question": {"text": questions[(qnum + 1) as usize].text, "responses": questions[(qnum + 1) as usize].responses.iter().map(|r| {json!({"text": r.text})}).collect::<Vec<Value>>()}}).to_string()));
                            }
                            Config::Slides(slides) => {
                                let next_slide = slides[(qnum + 1) as usize].clone();
                                match next_slide {
                                    SlidesItem::Question(question) => {
                                        for pl in &players {
                                            pl.do_send(PlayerCommand::SendText(json!({"event": "new_question", "question": {"text": question.text, "responses": question.responses.iter().map(|r| {json!({"text": r.text})}).collect::<Vec<Value>>()}}).to_string()));
                                        }
                                        teacher.do_send(TeacherCommand::SendText(json!({"event": "new_question", "question": {"text": question.text, "responses": question.responses.iter().map(|r| {json!({"text": r.text})}).collect::<Vec<Value>>()}}).to_string()));
                                    }
                                    SlidesItem::Slide(slide) => {
                                        teacher.do_send(TeacherCommand::SendText(
                                            json!({"event": "new_slide", "slide": slide})
                                                .to_string(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    
                }.into_actor(self));
            }
            GameCommand::StartGame => {
                let conf = self.config.clone();
                let session = self.teacher.clone();
                actix::spawn(async move {
                    match conf {
                        Config::Quiz { questions } => {
                             session
                            .do_send(TeacherCommand::SendText(json!({"event": "new_question", "question": {"text":questions[0].text, "responses": questions[0].responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>() }}).to_string()))

                        }
                        Config::Slides(slides) => match &slides[0] {
                            SlidesItem::Slide(slide) => {
                                 session
                                    .do_send(
                                      TeacherCommand::SendText(  json!({"event": "new_slide", "slide": slide})
                                            .to_string()),
                                    )

                            }
                            SlidesItem::Question(q) => {
                                 session
                                .do_send(TeacherCommand::SendText(json!({"event": "new_question", "question": {"text":q.text, "responses": q.responses.iter().map(move |r| {json!({"text": r.text})}).collect::<Vec<Value>>() }}).to_string()))
                            }
                        },
                    }
                });

            }
        }
        MessageResult(GameResponse::None)
    }
}

impl Handler<TeacherCommand> for Teacher {
    type Result = ();
    fn handle(&mut self, msg: TeacherCommand, _: &mut Self::Context) {
        match msg {
            TeacherCommand::SendText(r) => {
                let mut socket = self.socket.clone();
                actix::spawn(async move {
                    let _ = socket.text(r).await;
                });
            }
        }
    }
}

#[derive(Message)]
#[rtype(result = "()")]
enum TeacherCommand {
    SendText(String),
}

#[derive(Message)]
#[rtype(result = "PlayerResponse")]
enum PlayerCommand {
    SendText(String),
    QuestionCorrect,
    UpdateAnswer(u64),
    GetCurrAnswer,
    Pong(Bytes),
    GetLeaderboardData,
}

#[derive(Debug)]
enum PlayerResponse {
    CurrentAnswer(Option<u64>),
    LeaderboardData(String, u64),
    None,
}

impl Handler<PlayerCommand> for Player {
    type Result = MessageResult<PlayerCommand>;
    fn handle(&mut self, msg: PlayerCommand, _: &mut Self::Context) -> Self::Result {
        match msg {
            PlayerCommand::SendText(text) => {
                let mut socket = self.socket.clone();
                actix::spawn(async move {
                    let _ = socket.text(text).await;
                });
            }

            PlayerCommand::QuestionCorrect => {
                self.points += 600;
                self.curr_answer = None;
            }
            PlayerCommand::GetCurrAnswer => {
                return MessageResult(PlayerResponse::CurrentAnswer(self.curr_answer));
            }
            PlayerCommand::GetLeaderboardData => {
                return MessageResult(PlayerResponse::LeaderboardData(
                    self.name.clone(),
                    self.points,
                ));
            }

            PlayerCommand::UpdateAnswer(a) => {
                self.curr_answer = Some(a);
            }
            PlayerCommand::Pong(bytes) => {
                let mut socket = self.socket.clone();
                actix::spawn(async move {
                    let _ = socket.pong(&bytes).await;
                });
            }
        }
        MessageResult(PlayerResponse::None)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
enum Config {
    Slides(Vec<SlidesItem>),
    Quiz { questions: Vec<Question> },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
enum SlidesItem {
    Slide(Slide),
    Question(Question),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Slide {
    kind: SlideKind,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
enum SlideKind {
    Title(String, SlideStyle),
    TitleText(String, String, SlideStyle),
    OnlyText(String, SlideStyle),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct SlideStyle {
    color: String,
    font_color: String,
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
    state: web::Data<Addr<State>>,
) -> Result<HttpResponse, actix_web::Error> {
    let (res, session, stream) = actix_ws::handle(&req, stream)?;
    let (code, name) = path.into_inner();
    let game = {
        match state.send(StateCommand::GetGame(code)).await {
            Ok(StateResponse::Game(Some(ga))) => ga,
            _ => return Err(actix_web::error::ErrorNotFound("Game not found")),
        }
    };
    if name.is_inappropriate() {
        return Err(actix_web::error::ErrorForbidden("Name is Inappropriate"));
    }

    let name_clone = name.clone();
    let player = Player {
        name: name_clone,
        socket: session,
        points: 0,
        curr_answer: None,
    };
    let player_addr = player.start();
    game.do_send(GameCommand::AddPlayer(player_addr.clone()));

    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));
    actix_web::rt::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(AggregatedMessage::Text(text)) => {
                    match serde_json::from_str::<serde_json::Value>(&text) {
                        Ok(val) => match val["event"].as_str() {
                            Some("answer") => {
                                if let Some(answer) = val["answer"].as_u64() {
                                    if let Ok(GameResponse::Phase(GamePhase::OnQuestion(_))) =
                                        game.send(GameCommand::GetPhase).await
                                    {
                                        player_addr.do_send(PlayerCommand::UpdateAnswer(answer));
                                        player_addr.do_send(PlayerCommand::SendText(
                                            json!({ "event": "update_answer", "answer": answer })
                                                .to_string(),
                                        ));
                                    }
                                };
                            }
                            Some("joined") => {
                                player_addr
                                    .do_send(PlayerCommand::SendText("joinconfirmed".to_string()));
                                game.do_send(GameCommand::SendTextToTeacher(
                                    json!({ "event": "playerjoined", "player": { "name": name }})
                                        .to_string(),
                                ))
                            }
                            _ => {
                                player_addr.do_send(PlayerCommand::SendText(
                                    json!({ "error": "Command not found" }).to_string(),
                                ));
                            }
                        },
                        Err(_) => {
                            player_addr.do_send(PlayerCommand::SendText(
                                json!({ "error": "Command not found" }).to_string(),
                            ));
                        }
                    };
                }
                // Ok(AggregatedMessage::Binary(bin)) => {
                //     let mut session = session_clone.lock().await;
                //     session.binary(bin).await.unwrap();
                // }
                Ok(AggregatedMessage::Ping(msg)) => {
                    player_addr.do_send(PlayerCommand::Pong(msg));
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
    state: web::Data<Addr<State>>,
    path: web::Path<(String, String)>,
) -> Result<HttpResponse, actix_web::Error> {
    let (res, mut session, stream) = actix_ws::handle(&req, stream)?;

    let (app_key, game_id) = path.into_inner();

    let code = rand::rng().random_range(1000..=9999).to_string();
    let activity = {
        match state.send(StateCommand::GetActivity(game_id)).await {
            Ok(StateResponse::Activity(Some(activity))) => activity.clone(),
            _ => return Err(actix_web::error::ErrorNotFound("Set not found")),
        }
    };
    let access: bool = match activity.access.clone() {
        Access::All(_) => true,
        Access::Usernames(v) => {
            let mut access = false;
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
                                access = true;
                            }
                        }
                    }
                }
            }
            access
        }
    };
    if !access {
        return Err(actix_web::error::ErrorForbidden(
            "You do not have access to this set",
        ));
    }
    let g = Game {
        config: activity.config,
        phase: GamePhase::Lobby,
        players: vec![],
        teacher: Teacher {
            socket: session.clone(),
        }
        .start(),
    }
    .start();
    state.do_send(StateCommand::CreateGame(code.clone(), g.clone()));

    if session
        .text(
            json!(
                { "event": "gamecode",  "gamecode": code.clone() }
            )
            .to_string(),
        )
        .await.is_err()
    {
            state.do_send(StateCommand::DeleteGame(code.clone()));
    }

    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));
    actix_web::rt::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(AggregatedMessage::Text(text)) => {
                    if text == "gamestart" {
state.do_send(StateCommand::StartGame(code.clone()));
                    } else if text == "continue" {
                        println!("Continuing");
                        g.do_send(GameCommand::Continue);
                    }
                }
                Ok(AggregatedMessage::Binary(bin)) => {
                    if session.binary(bin).await.is_err() {
                        g.do_send(GameCommand::TeacherDisconnect);
                    };
                }
                Ok(AggregatedMessage::Ping(msg)) => {
                    if session.pong(&msg).await.is_err() {
                        g.do_send(GameCommand::TeacherDisconnect);
                    };
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
    state: web::Data<Addr<State>>,
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

                id_list.push(json!(id.clone()));

                let _ = reqwest::Client::new()
                    .post(format!("https://www.vortice.app/api/apps/{}", app_key))
                    .body(json!({ "data": id_list }).to_string())
                    .send()
                    .await;

                state.do_send(StateCommand::CreateActivity(
                    id.clone(),
                    Activity {
                        name: data.name,
                        access: data.access,
                        config: data.config,
                    },
                ));
                state.do_send(StateCommand::SaveToFile);

                return HttpResponse::Ok().json(json!({ "status": "Success", "id": id }));
            }
        }
    }

    ugg("Error with Vortice services")
}

#[get("/api/{app_key}/get_sets")]
async fn get_sets(path: web::Path<String>, state: web::Data<Addr<State>>) -> impl Responder {
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
                    if let Ok(aaa) = serde_json::from_str::<Value>(r) {
                        if let Some(r) = aaa["data"].as_array() {
                            for val in r {
                                if let Some(l) = val.as_str() {
                                    if let Ok(StateResponse::Activity(Some(e))) =
                                        state.send(StateCommand::GetActivity(l.to_string())).await
                                    {
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

#[post("/api/teacher/{app_key}/delete_set/{set_id}")]
async fn delete_set(path: web::Path<(String, String)>) -> impl Responder {
    let (app_key, set_id) = path.into_inner();
    
    "Hello, World!".to_string()
}

#[get("/api/ws/get_error/{code}/{name}")]
async fn get_error(
    path: web::Path<(String, String)>,
    state: web::Data<Addr<State>>,
) -> impl Responder {
    let (code, name) = path.into_inner();
    if name.is_inappropriate() {
        return ugg("Name is inappropriate");
    }
    match state.send(StateCommand::GetGame(code)).await {
        Ok(_) => ugg("Unknown error, try again later"),
        _ => ugg("Game not found, try again later"),
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
        match std::fs::read_to_string(Path::new("./activities.txt")) {
            Ok(v) => v,
            Err(_) => "{}".to_string(),
        }
        .as_str(),
    )
    .unwrap_or_default();
    let state: Addr<State> = State::new(exist).start();
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
