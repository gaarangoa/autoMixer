use automixer_lib::{config::Config, web::run_remote_server};

#[tokio::main]
async fn main() {
    if let Err(error) = run_remote_server(Config::load()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
