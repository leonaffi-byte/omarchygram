//! Read-only real-account media diagnostic. Run only through bin/headless,
//! with Omarchygram quit. Reports outcomes, never names or message contents.
use gstreamer::{self as gst, prelude::*};
use gtk4::gio::prelude::FileExt;
use omarchygram::tg::{AuthState, MediaKind, Tg};
use std::{path::PathBuf, time::Duration};

fn decode_voice(path: &std::path::Path) -> Result<(), String> {
    gst::init().map_err(|e| e.to_string())?;
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .build()
        .map_err(|e| e.to_string())?;
    let player = gst::ElementFactory::make("playbin3")
        .property("uri", gtk4::gio::File::for_path(path).uri().to_string())
        .property("audio-sink", &sink)
        .build()
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let bus = player.bus().ok_or("No playback bus")?;
        player
            .set_state(gst::State::Playing)
            .map_err(|e| e.to_string())?;
        match bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Error, gst::MessageType::Eos],
        ) {
            Some(message) => match message.view() {
                gst::MessageView::Eos(_) => Ok(()),
                gst::MessageView::Error(e) => Err(e.error().to_string()),
                _ => unreachable!(),
            },
            None => Err("Audio decoding timed out".into()),
        }
    })();
    let _ = player.set_state(gst::State::Null);
    result
}

async fn downloaded(
    name: &str,
    future: impl std::future::Future<Output = Result<Option<PathBuf>, String>>,
    image: bool,
) -> bool {
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(35), future).await;
    match result {
        Ok(Ok(Some(path))) => {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if image {
                match gtk4::gdk_pixbuf::Pixbuf::from_file(&path) {
                    Ok(pixbuf) => println!(
                        "ok   {name}: {bytes} bytes, decoded {}x{} ({:.2}s)",
                        pixbuf.width(),
                        pixbuf.height(),
                        started.elapsed().as_secs_f64()
                    ),
                    Err(error) => {
                        eprintln!("FAIL {name}: image decode: {error}");
                        return false;
                    }
                }
                if let Err(error) = gtk4::gdk::Texture::from_filename(&path) {
                    eprintln!("FAIL {name}: viewer texture decode: {error}");
                    return false;
                }
            } else {
                if let Err(error) = decode_voice(&path) {
                    eprintln!("FAIL {name}: audio decode: {error}");
                    return false;
                }
                println!(
                    "ok   {name}: {bytes} bytes, GStreamer decoded to end without audio output ({:.2}s)",
                    started.elapsed().as_secs_f64()
                );
            }
            bytes > 0
        }
        Ok(Ok(None)) => {
            eprintln!("FAIL {name}: no downloadable media");
            false
        }
        Ok(Err(error)) => {
            eprintln!("FAIL {name}: {error}");
            false
        }
        Err(_) => {
            eprintln!("FAIL {name}: timed out after 35s");
            false
        }
    }
}

async fn probe(tg: &Tg) -> Result<bool, String> {
    if tg.start().await? != AuthState::Ready {
        return Err("Existing session is not authorized".into());
    }
    let dialogs = tg.get_dialogs().await?;
    println!("ok   authorized, {} dialogs", dialogs.len());
    let mut success = true;
    for (index, chat) in dialogs.iter().filter(|c| c.has_photo).take(3).enumerate() {
        success &= downloaded(
            &format!("avatar {index}"),
            tg.download_avatar(chat.id),
            true,
        )
        .await;
        success &= downloaded(
            &format!("profile photo {index}"),
            tg.download_profile_photo(chat.id),
            true,
        )
        .await;
    }
    let (mut photo, mut voice) = (None, None);
    for chat in dialogs.iter().take(20) {
        for msg in tg.get_history(chat.id, None).await? {
            match msg.media {
                Some(MediaKind::Photo) if photo.is_none() => photo = Some((chat.id, msg.id)),
                Some(MediaKind::Voice) if voice.is_none() => voice = Some((chat.id, msg.id)),
                _ => {}
            }
        }
        if photo.is_some() && voice.is_some() {
            break;
        }
    }
    for (name, message, image) in [
        ("message photo", photo, true),
        ("voice message", voice, false),
    ] {
        if let Some((chat, msg)) = message {
            success &= downloaded(name, tg.download_media(chat, msg), image).await;
        } else {
            println!("SKIP {name}: no recent example found");
        }
    }
    Ok(success)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    if !std::env::var("WAYLAND_DISPLAY").is_ok_and(|value| value.starts_with("wayland-omg-")) {
        eprintln!("Run this diagnostic through bin/headless.");
        return std::process::ExitCode::FAILURE;
    }
    if std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == "omarchygram")
        })
    {
        eprintln!("Quit Omarchygram before running this diagnostic.");
        return std::process::ExitCode::FAILURE;
    }
    if gtk4::init().is_err() {
        eprintln!("Could not initialize headless GTK.");
        return std::process::ExitCode::FAILURE;
    }
    let tg = Tg::spawn_real();
    let result = tokio::time::timeout(Duration::from_secs(300), probe(&tg)).await;
    tg.shutdown().await;
    match result {
        Ok(Ok(true)) => std::process::ExitCode::SUCCESS,
        Ok(Ok(false)) => std::process::ExitCode::FAILURE,
        Ok(Err(error)) => {
            eprintln!("FAIL diagnostic: {error}");
            std::process::ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("FAIL diagnostic: overall timeout");
            std::process::ExitCode::FAILURE
        }
    }
}
