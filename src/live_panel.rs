//! Native controls around a volatile, bounded preview of this recording.
use crate::{
    live::{Session, Update},
    live_buffer::Context,
    settings,
};
use adw::prelude::*;
use gtk::{gio, glib};
use meeting_local_assistant::process::Control;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

pub struct Panel {
    pub toggle: adw::SwitchRow,
    pub widget: gtk::Box,
    model: gtk::Entry,
    question: gtk::Entry,
    ask: gtk::Button,
    recap: gtk::Button,
    preview: gtk::Label,
    answer: gtk::Label,
    context: RefCell<Context>,
    session: RefCell<Option<Session>>,
    request: RefCell<Option<Control>>,
    generation: Cell<u64>,
    active: Cell<bool>,
    paused: Cell<bool>,
    title: RefCell<String>,
}

impl Panel {
    pub fn new() -> Rc<Self> {
        let toggle = adw::SwitchRow::builder()
            .title("Live meeting assistant")
            .subtitle(
                "Uses your existing Whisper model. Provisional text; extra memory and GPU/CPU use.",
            )
            .active(settings::live_assistant_enabled())
            .build();
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .visible(false)
            .build();
        let heading = gtk::Label::builder()
            .label("Live assistant · recent provisional transcript")
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        widget.append(&heading);
        let preview = gtk::Label::builder()
            .label("Waiting for speech…")
            .wrap(true)
            .selectable(true)
            .xalign(0.0)
            .valign(gtk::Align::Start)
            .build();
        widget.append(
            &gtk::ScrolledWindow::builder()
                .child(&preview)
                .min_content_height(90)
                .max_content_height(90)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        let model = gtk::Entry::builder().text(settings::assistant_model())
            .placeholder_text("Installed local Ollama model").tooltip_text("Local model for answers; speech-to-text uses the recorder’s existing Whisper model").build();
        widget.append(&model);
        let question = gtk::Entry::builder()
            .placeholder_text("Ask about the recent conversation…")
            .max_length(1000)
            .build();
        widget.append(&question);
        let buttons = gtk::Box::builder().spacing(8).build();
        let ask = gtk::Button::with_label("Ask");
        let recap = gtk::Button::with_label("Recap recent discussion");
        buttons.append(&ask);
        buttons.append(&recap);
        widget.append(&buttons);
        let answer = gtk::Label::builder()
            .wrap(true)
            .selectable(true)
            .xalign(0.0)
            .valign(gtk::Align::Start)
            .build();
        widget.append(
            &gtk::ScrolledWindow::builder()
                .child(&answer)
                .min_content_height(100)
                .max_content_height(100)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        let panel = Rc::new(Self {
            toggle,
            widget,
            model,
            question,
            ask,
            recap,
            preview,
            answer,
            context: RefCell::default(),
            session: RefCell::default(),
            request: RefCell::default(),
            generation: Cell::new(0),
            active: Cell::new(false),
            paused: Cell::new(false),
            title: RefCell::default(),
        });
        panel
            .toggle
            .connect_active_notify(|row| settings::save_live_assistant(row.is_active()));
        panel
            .model
            .connect_changed(|entry| settings::save_assistant_model(entry.text().as_str()));
        let weak = Rc::downgrade(&panel);
        panel.ask.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.query(false);
            }
        });
        let weak = Rc::downgrade(&panel);
        panel.question.connect_activate(move |_| {
            if let Some(p) = weak.upgrade() {
                p.query(false);
            }
        });
        let weak = Rc::downgrade(&panel);
        panel.recap.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.query(true);
            }
        });
        panel
    }

    pub fn start(self: &Rc<Self>, dir: &Path, language: &'static str, title: String) {
        self.active.set(true);
        self.paused.set(false);
        self.context.replace(Context::default());
        self.title.replace(title);
        self.preview.set_label("Waiting for speech…");
        self.answer.set_label("");
        self.question.set_text("");
        self.widget.set_visible(self.toggle.is_active());
        if !self.toggle.is_active() {
            return;
        }
        let generation = self.generation.get();
        let (tx, rx) = async_channel::bounded(1);
        match Session::start(dir, language, tx) {
            Ok(session) => {
                self.session.replace(Some(session));
            }
            Err(error) => {
                self.preview.set_label(&error);
                return;
            }
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(update) = rx.recv().await {
                let Some(panel) = weak.upgrade() else {
                    break;
                };
                if panel.generation.get() != generation || !panel.active.get() {
                    break;
                }
                match update {
                    Update::Ready => panel.preview.set_label("Waiting for speech…"),
                    Update::Lines(lines) if !panel.paused.get() => {
                        for line in lines {
                            panel.context.borrow_mut().push(line);
                        }
                        let text = panel.context.borrow().text();
                        panel.preview.set_label(&text);
                    }
                    Update::Error(error) => panel.preview.set_label(&format!(
                        "Live transcription stopped: {error}. Recording continues."
                    )),
                    _ => {}
                }
            }
        });
    }

    pub fn pause(&self, paused: bool) {
        self.paused.set(paused);
        if let Some(session) = self.session.borrow().as_ref() {
            session.set_paused(paused);
        }
        if paused {
            self.cancel_request();
        }
        self.ask.set_sensitive(!paused);
        self.recap.set_sensitive(!paused);
    }

    fn cancel_request(&self) {
        if let Some(control) = self.request.borrow_mut().take() {
            control
                .cancel
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    pub fn stop(&self) -> Option<Session> {
        self.active.set(false);
        self.generation.set(self.generation.get().wrapping_add(1));
        self.cancel_request();
        self.context.replace(Context::default());
        self.preview.set_label("");
        self.answer.set_label("");
        self.question.set_text("");
        self.title.borrow_mut().clear();
        self.widget.set_visible(false);
        self.ask.set_sensitive(true);
        self.recap.set_sensitive(true);
        let session = self.session.borrow_mut().take();
        if let Some(session) = &session {
            session.cancel();
        }
        session
    }

    fn query(self: &Rc<Self>, recap: bool) {
        if !self.active.get() || self.paused.get() || self.request.borrow().is_some() {
            return;
        }
        let context = self.context.borrow().text();
        if context.is_empty() {
            self.answer
                .set_label("Wait for a provisional transcript first.");
            return;
        }
        let question = if recap {
            "Summarise the recent discussion, decisions and explicitly agreed actions.".to_owned()
        } else {
            self.question.text().trim().to_owned()
        };
        if question.is_empty() || question.len() > 1000 {
            self.answer
                .set_label("Enter a question of at most 1,000 bytes.");
            return;
        }
        let model = self.model.text().trim().to_owned();
        let title = self.title.borrow().clone();
        let generation = self.generation.get();
        let control = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(120));
        self.request.replace(Some(control.clone()));
        self.ask.set_sensitive(false);
        self.recap.set_sensitive(false);
        self.answer.set_label("Thinking locally…");
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let work_control = control.clone();
            let result = gio::spawn_blocking(move || {
                meeting_local_assistant::live_answer(
                    &title,
                    &context,
                    &question,
                    &model,
                    &work_control,
                )
            })
            .await;
            let Some(panel) = weak.upgrade() else {
                return;
            };
            let result = result.unwrap_or_else(|_| {
                Err(meeting_local_assistant::model::ApiError::new(
                    "WORKER",
                    "The local assistant worker stopped unexpectedly.",
                ))
            });
            panel.complete(generation, &control, result);
        });
    }

    fn complete(
        &self,
        generation: u64,
        control: &Control,
        result: Result<String, meeting_local_assistant::model::ApiError>,
    ) {
        if self.generation.get() != generation
            || control.cancel.load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        self.request.borrow_mut().take();
        self.ask.set_sensitive(!self.paused.get());
        self.recap.set_sensitive(!self.paused.get());
        match result {
            Ok(text) => self.answer.set_label(&format!(
                "Based on a snapshot of the recent provisional transcript:\n\n{text}"
            )),
            Err(error) => self.answer.set_label(&format!("Local assistant: {error}")),
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.cancel_request();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a GTK display; preview CI runs this under Xvfb"]
    fn native_panel_rejects_stale_answers_and_clears_on_stop() {
        gtk::init().unwrap();
        let panel = Panel::new();
        panel.active.set(true);
        panel.widget.set_visible(true);
        panel
            .context
            .borrow_mut()
            .push("[00:01] You: private".into());
        panel.preview.set_label("private");
        panel.answer.set_label("Thinking locally…");
        let generation = panel.generation.get();
        let control = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(1));
        panel.request.replace(Some(control.clone()));
        let weak = Rc::downgrade(&panel);
        glib::idle_add_local_once(move || {
            weak.upgrade().unwrap().complete(
                generation,
                &control,
                Ok("late private answer".into()),
            );
        });
        panel.stop();
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        assert_eq!(panel.preview.text(), "");
        assert_eq!(panel.answer.text(), "");
        assert!(panel.context.borrow().text().is_empty());
        assert!(!panel.widget.is_visible());
        assert!(panel.request.borrow().is_none());

        panel.active.set(true);
        let control = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(1));
        panel.request.replace(Some(control.clone()));
        panel.pause(true);
        panel.pause(false);
        panel.complete(
            panel.generation.get(),
            &control,
            Ok("cancelled answer".into()),
        );
        assert_eq!(panel.answer.text(), "");
        assert!(panel.ask.is_sensitive());
        let control = Control::new(Arc::new(AtomicBool::new(false)), Duration::from_secs(1));
        panel.request.replace(Some(control.clone()));
        panel.complete(
            panel.generation.get(),
            &control,
            Err(meeting_local_assistant::model::ApiError::new(
                "TIMEOUT",
                "fixture timeout",
            )),
        );
        assert!(panel.answer.text().contains("TIMEOUT"));
        assert!(panel.ask.is_sensitive());
        assert!(panel.request.borrow().is_none());
    }
}
