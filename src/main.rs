use crate::extractor::run_extractor;

mod extractor;
mod server;
mod util;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let extractor = tokio::spawn(run_extractor());
    let server = tokio::spawn(run_server());
    tokio::select! {
        res = extractor => res??,
        res = server => res??,
    }
    Ok(())
}

async fn run_server() -> anyhow::Result<()> {
    let app = server::router();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:6969").await?;
    println!("Serving on http://0.0.0.0:6969");
    axum::serve(listener, app).await?;
    Ok(())
}
