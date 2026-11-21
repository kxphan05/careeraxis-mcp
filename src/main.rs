mod browser;
mod cli;
mod client;
mod html;
mod mcp;
mod session;
mod tools;

use std::time::Duration;

use client::Client;
use session::Jar;
use tools::App;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(args).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            if format!("{e:#}").contains(client::NEEDS_LOGIN) { 3 } else { 1 }
        }
    };
    std::process::exit(code);
}

async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let app = || -> anyhow::Result<App> { Ok(App::new(Client::new(Jar::load_or_default(), true)?)) };
    match args.first().map(String::as_str) {
        None | Some("mcp") | Some("serve") => mcp::serve(app()?).await,
        Some("help") | Some("-h") | Some("--help") => {
            print!("{}", cli::help());
            Ok(())
        }
        Some("-V") | Some("--version") | Some("version") => {
            println!("careeraxis-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("setup") | Some("login") => {
            eprintln!("Opening a browser window — sign in to NTU (tick \"Keep me signed in\").");
            let jar = browser::interactive_login(Duration::from_secs(15 * 60)).await?;
            jar.save()?;
            eprintln!("Saved session to {}", session::jar_path().display());
            let app = App::new(Client::new(jar, true)?);
            cli::run(&app, &["status".to_string()]).await
        }
        Some(_) => cli::run(&app()?, &args).await,
    }
}
