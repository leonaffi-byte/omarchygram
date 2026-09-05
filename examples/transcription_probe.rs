//! Provider diagnostic for an explicitly supplied audio fixture; no Telegram session.
//! bin/headless cargo run --example transcription_probe -- fixture.bin
use omarchygram::ai::{self, Prefs};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("Supply the path of an audio fixture to transcribe with Groq.");
        return std::process::ExitCode::FAILURE;
    };
    let prefs = Prefs { transcribe_provider: "groq".into(), ..Prefs::default() };
    match ai::transcribe(&prefs, std::path::Path::new(&path)).await {
        Ok(transcript) => {
            println!("PASS {}: {} transcript characters", transcript.provider, transcript.text.chars().count());
            std::process::ExitCode::SUCCESS
        }
        Err(error) => { eprintln!("FAIL {error}"); std::process::ExitCode::FAILURE }
    }
}
