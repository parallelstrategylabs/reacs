use eframe::egui::Context;
use reacs::tracking::{TrackerMessage, TrackingSave};
use std::io::ErrorKind;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

pub enum Source {
    File(PathBuf),
    Port(u16),
}

pub enum Event {
    Connected,
    Message(TrackerMessage),
    Closed(Option<String>),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::File(path) => path.display().to_string(),
            Source::Port(port) => format!("port {port}"),
        }
    }

    pub fn open_file(&self) -> Result<Option<TrackingSave>, String> {
        let Source::File(path) = self else {
            return Ok(None);
        };
        let bytes = std::fs::read(path)
            .map_err(|error| format!("can't read `{}`: {error}", path.display()))?;
        TrackingSave::from_bytes(&bytes)
            .map(Some)
            .ok_or_else(|| format!("`{}` is not a tracking save", path.display()))
    }

    pub fn listen(&self, context: Context) -> Option<Receiver<Event>> {
        let Source::Port(port) = *self else {
            return None;
        };

        let (sender, receiver) = channel();
        thread::spawn(move || {
            let mut stream = loop {
                match TcpStream::connect(("127.0.0.1", port)) {
                    Ok(stream) => break stream,
                    Err(_) => thread::sleep(Duration::from_millis(100)),
                }
            };
            let _ = sender.send(Event::Connected);
            context.request_repaint();

            loop {
                let event = match TrackerMessage::read_from(&mut stream) {
                    Ok(message) => Event::Message(message),
                    Err(error) if error.kind() == ErrorKind::UnexpectedEof => Event::Closed(None),
                    Err(error) => Event::Closed(Some(error.to_string())),
                };
                let closed = matches!(event, Event::Closed(_));
                if sender.send(event).is_err() {
                    return;
                }
                if closed {
                    context.request_repaint();
                    return;
                }
            }
        });
        Some(receiver)
    }
}
