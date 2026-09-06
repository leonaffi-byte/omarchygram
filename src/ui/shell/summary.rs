use super::*;
use crate::ai::summary::{Range, chunks, entry};

#[derive(Clone)]
pub(super) struct Selection {
    chat_id: i64,
    first: i32,
    last: Option<i32>,
}

impl ShellInner {
    pub(super) async fn probe_range_summary(self: &Rc<Self>, mom: i64, marta: i64) -> bool {
        let panel = self.messages.summary_panel();
        self.aux.borrow_mut().transcripts.remove(&(mom, 301));
        probe_step("range summary uses actual message menu actions");
        if !self
            .messages
            .probe_message_menu_click(302, "Summarize from here")
            || panel.first.get() != Some(302)
            || !self
                .messages
                .probe_message_menu_click(301, "Summarize to here")
        {
            probe_fail("summary endpoint menus");
            return false;
        }
        probe_step("range summary cancellation stops owned transcription");
        if !poll_until(4000, || self.summary_voice.get() == Some((mom, 301))).await {
            probe_fail("summary voice started");
            return false;
        }
        panel.click_close();
        glib::timeout_future(Duration::from_millis(400)).await;
        if self.summary_task.borrow().is_some()
            || self.transcription_active.borrow().contains_key(&(mom, 301))
            || matches!(
                self.aux.borrow().transcripts.get(&(mom, 301)),
                Some(ReqState::Done(_))
            )
            || panel.widget.is_visible()
        {
            probe_fail("summary cancellation left background work");
            return false;
        }
        probe_step("reverse endpoints include voice and exclude messages outside range");
        self.select_summary_start(Some(302));
        self.clone().select_summary_end(301);
        if !poll_until(5000, || self.summary_task.borrow().is_none()).await
            || !panel.text().contains("[voice transcript] (mock transcript")
            || !panel.text().contains("#301 ·")
            || !panel.text().contains("#302 ·")
            || panel.text().contains("#303 ·")
        {
            probe_fail(&format!("inclusive voice summary: {}", panel.detail()));
            return false;
        }
        self.probe_capture_widget("chat-range-summary-panel", panel.widget.upcast_ref())
            .await;
        let cached = self.aux.borrow().transcripts.get(&(mom, 301)).cloned();
        let provider = self.settings.get().ai.chat_provider;
        self.settings
            .update(|s| s.ai.chat_provider = "unavailable-probe-provider".into());
        probe_step("missing summary provider has visible error and Retry");
        self.select_summary_start(Some(301));
        self.clone().select_summary_end(302);
        if !poll_until(2000, || self.summary_task.borrow().is_none()).await
            || !panel.detail().contains("No chat AI provider")
        {
            probe_fail("summary missing provider feedback");
            return false;
        }
        self.settings.update(|s| s.ai.chat_provider = provider);
        panel.click_retry();
        if !poll_until(4000, || self.summary_task.borrow().is_none()).await
            || !panel.text().contains("[voice transcript]")
            || self.aux.borrow().transcripts.get(&(mom, 301)).cloned() != cached
        {
            probe_fail("summary retry reuses transcript");
            return false;
        }

        probe_step("summary range includes an unloaded history gap");
        self.clone().open_chat(marta);
        if !poll_until(3000, || {
            self.open_chat.get() == Some(marta) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("summary historical chat");
            return false;
        }
        let old = self
            .tg
            .get_history(marta, Some(92))
            .await
            .unwrap_or_default();
        let Some(first) = old.into_iter().find(|m| m.id == 90) else {
            probe_fail("summary old endpoint");
            return false;
        };
        self.messages.merge_event(first);
        self.select_summary_start(Some(90));
        self.clone().select_summary_end(100);
        if !poll_until(5000, || self.summary_task.borrow().is_none()).await
            || !panel.text().contains("#90 ·")
            || !panel.text().contains("#91 ·")
            || !panel.text().contains("#100 ·")
            || panel.text().contains("#101 ·")
        {
            probe_fail("summary skipped unloaded range messages");
            return false;
        }
        self.clone().open_chat(mom);
        if !poll_until(3000, || {
            self.open_chat.get() == Some(mom) && !self.messages.is_loading()
        })
        .await
        {
            return false;
        }
        probe_step("switching chat cancels summary and prevents stale result");
        self.select_summary_start(Some(301));
        self.clone().select_summary_end(302);
        if !poll_until(4000, || panel.detail().starts_with("Writing summary")).await {
            probe_fail("summary request reached provider before chat switch");
            return false;
        }
        self.clone().open_chat(marta);
        if self.summary_task.borrow().is_some() || panel.widget.is_visible() {
            probe_fail("summary survived chat switch");
            return false;
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        if panel.widget.is_visible() {
            probe_fail("late summary leaked into another chat");
            return false;
        }
        self.clone().open_chat(mom);
        if !poll_until(3000, || {
            self.open_chat.get() == Some(mom) && !self.messages.is_loading()
        })
        .await
        {
            return false;
        }
        probe_step("disabling AI clears range selection and pending request");
        self.select_summary_start(Some(301));
        self.clone().select_summary_end(302);
        self.settings.update(|s| s.ai.enabled = false);
        if self.summary_task.borrow().is_some()
            || panel.widget.is_visible()
            || self.summary_range.borrow().is_some()
        {
            probe_fail("summary survived disabling AI");
            return false;
        }
        self.settings.update(|s| s.ai.enabled = true);
        true
    }

    pub(super) fn close_summary(&self) {
        if let Some(task) = self.summary_task.borrow_mut().take() {
            task.abort();
        }
        if let Some(key) = self.summary_voice.take() {
            if let Some(job) = self.transcription_active.borrow_mut().remove(&key) {
                job.task.abort();
            }
            if matches!(
                self.aux.borrow().transcripts.get(&key),
                Some(ReqState::InFlight)
            ) {
                self.aux.borrow_mut().transcripts.insert(
                    key,
                    ReqState::Failed("Summary cancelled. Use Transcribe to retry.".into()),
                );
            }
        }
        self.summary_range.borrow_mut().take();
        let panel = self.messages.summary_panel();
        if panel.widget.is_visible() {
            self.messages.move_focus_before_removal(&panel.widget);
        }
        panel.hide();
    }

    pub(super) fn select_summary_start(&self, msg_id: Option<i32>) {
        if !self.session_ready.get() || !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let message = msg_id
            .and_then(|id| self.messages.message(id))
            .filter(|message| message.id > 0 && !message.deleted);
        if msg_id.is_some() && message.is_none() {
            return;
        }
        self.close_summary();
        let text = if let Some(message) = &message {
            *self.summary_range.borrow_mut() = Some(Selection {
                chat_id,
                first: message.id,
                last: None,
            });
            format!(
                "First: {} · {} · #{}. Right-click the last message → Summarize to here. Both endpoints and voice transcripts are included.",
                message.sender,
                message.ts.format("%b %d %H:%M"),
                message.id
            )
        } else {
            "Right-click the first message → Summarize from here, then the last → Summarize to here. Includes both endpoints and voice transcripts. Uses your configured AI provider.".into()
        };
        self.messages
            .summary_panel()
            .select(message.map(|m| m.id), &text);
    }

    pub(super) fn select_summary_end(self: Rc<Self>, msg_id: i32) {
        if self
            .messages
            .message(msg_id)
            .is_none_or(|m| m.id <= 0 || m.deleted)
        {
            return;
        }
        {
            let mut selection = self.summary_range.borrow_mut();
            let Some(selection) = selection
                .as_mut()
                .filter(|s| Some(s.chat_id) == self.open_chat.get())
            else {
                return;
            };
            selection.last = Some(msg_id);
        }
        self.start_summary();
    }

    pub(super) fn start_summary(self: Rc<Self>) {
        if !self.session_ready.get()
            || !self.settings.get().ai.enabled
            || self.summary_task.borrow().is_some()
        {
            return;
        }
        let Some(selection) = self
            .summary_range
            .borrow()
            .clone()
            .filter(|s| Some(s.chat_id) == self.open_chat.get() && s.last.is_some())
        else {
            return;
        };
        let panel = self.messages.summary_panel();
        self.messages.move_focus_before_removal(&panel.widget);
        panel.progress("Checking AI provider…");
        let session = self.session_epoch.get();
        let weak = Rc::downgrade(&self);
        let task = glib::MainContext::default().spawn_local(async move {
            let Some(this) = weak.upgrade() else { return };
            let result = this.run_summary(&selection).await;
            if !this.is_session_current(session) || this.open_chat.get() != Some(selection.chat_id)
            {
                return;
            }
            this.summary_task.borrow_mut().take();
            let panel = this.messages.summary_panel();
            this.messages.move_focus_before_removal(&panel.widget);
            match result {
                Ok((text, count, voices)) => panel.finish(
                    &format!(
                        "Chat summary · {count} {} · {voices} {}",
                        if count == 1 { "message" } else { "messages" },
                        if voices == 1 {
                            "voice note"
                        } else {
                            "voice notes"
                        }
                    ),
                    Ok(&text),
                ),
                Err(error) => panel.finish("Summary failed", Err(&error)),
            }
        });
        *self.summary_task.borrow_mut() = Some(task);
    }

    async fn run_summary(
        self: &Rc<Self>,
        selection: &Selection,
    ) -> Result<(String, usize, usize), String> {
        let prefs = ai_prefs(&self.settings.get());
        let providers = self.local.detect(prefs.clone()).await;
        let pinned = prefs.chat_provider.trim().to_lowercase();
        if !providers.iter().any(|p| {
            p.task == crate::ai::Task::Chat && p.available && (pinned.is_empty() || p.id == pinned)
        }) {
            return Err("No chat AI provider is available. Check Settings → AI and Assistant /status, then Retry. Add an API key under [ai] in config.toml or run Ollama locally.".into());
        }
        let panel = self.messages.summary_panel();
        let mut range = Range::new(selection.chat_id, selection.first, selection.last.unwrap())?;
        let mut before = range
            .last
            .checked_add(1)
            .ok_or("Choose a delivered message as the last endpoint.")?;
        loop {
            panel.progress(&format!(
                "Loading selected range… {} messages",
                range.message_count()
            ));
            let mut page = self.tg.get_history(selection.chat_id, Some(before)).await?;
            self.apply_tombstones(selection.chat_id, &mut page);
            let oldest = page
                .iter()
                .filter(|m| msg_in_chat(m, selection.chat_id) && m.id > 0 && m.id < before)
                .map(|m| m.id)
                .min();
            range.add(page)?;
            match oldest {
                Some(id) if id <= range.first => break,
                Some(id) => before = id,
                None => break,
            }
        }
        let messages = range.finish()?;
        let count = messages.len();
        let voices = messages
            .iter()
            .filter(|m| !m.deleted && m.media == Some(MediaKind::Voice))
            .count();
        let title = self.title_for(selection.chat_id);
        let mut entries = Vec::with_capacity(count);
        let mut voice_index = 0;
        for message in messages {
            let voice = if !message.deleted && message.media == Some(MediaKind::Voice) {
                voice_index += 1;
                panel.progress(&format!(
                    "Transcribing voice {voice_index} of {voices}… · {} messages",
                    count
                ));
                Some(self.summary_transcript(selection.chat_id, message.id).await
                    .map_err(|e| format!("Voice #{} could not be transcribed: {e}\nRetry includes the full range and reuses completed transcripts.", message.id))?)
            } else {
                None
            };
            entries.push((message, voice));
        }
        panel.progress("Preparing summary…");
        let mut parts = gio::spawn_blocking(move || {
            let mut transcript = String::new();
            for (message, voice) in entries {
                transcript.push_str(&entry(&message, voice.as_deref())?);
            }
            chunks(&transcript)
        })
        .await
        .map_err(|_| "Could not prepare the selected messages.".to_string())??;
        // Reduce long ranges in bounded prompts. No old messages are dropped to
        // fit a provider's context window; each part feeds the final synthesis.
        for round in 0..8 {
            if parts.len() == 1 {
                panel.progress(&format!(
                    "Writing summary… · {count} messages · {voices} voice notes"
                ));
                let text = self.summary_reply(&prefs, &title, &parts[0], false).await?;
                return Ok((text, count, voices));
            }
            let mut notes = String::new();
            for (index, part) in parts.iter().enumerate() {
                panel.progress(&format!(
                    "Summarizing part {} of {}… · pass {}",
                    index + 1,
                    parts.len(),
                    round + 1
                ));
                let reply = self.summary_reply(&prefs, &title, part, true).await?;
                notes.push_str(&format!("Part {}:\n{reply}\n\n", index + 1));
            }
            parts = gio::spawn_blocking(move || chunks(&notes))
                .await
                .map_err(|_| "Could not combine summary parts.".to_string())??;
        }
        Err("The AI provider did not condense this range enough. Choose a shorter range or another model, then Retry.".into())
    }

    async fn summary_reply(
        &self,
        prefs: &Prefs,
        title: &str,
        text: &str,
        partial: bool,
    ) -> Result<String, String> {
        let (system, user) = prompts::summarize_range(title, text, partial);
        let reply = self
            .local
            .chat(
                prefs.clone(),
                system,
                vec![ChatMessage {
                    role: Role::User,
                    content: user,
                }],
            )
            .await?;
        if reply.text.trim().is_empty() {
            return Err(
                "The AI provider returned an empty summary. Retry or choose another model.".into(),
            );
        }
        Ok(reply.text)
    }

    async fn summary_transcript(
        self: &Rc<Self>,
        chat_id: i64,
        msg_id: i32,
    ) -> Result<String, String> {
        let key = (transcription_chat(chat_id), msg_id);
        if let Some(ReqState::Done(text)) = self.aux.borrow().transcripts.get(&key)
            && !text.trim().is_empty()
        {
            return Ok(text.clone());
        }
        if !self.transcription_active.borrow().contains_key(&key) {
            self.transcription_queue
                .borrow_mut()
                .retain(|queued| *queued != key);
            self.aux
                .borrow_mut()
                .transcripts
                .insert(key, ReqState::InFlight);
            self.summary_voice.set(Some(key));
            self.start_transcription(key, false);
        }
        let result = loop {
            match self.aux.borrow().transcripts.get(&key) {
                Some(ReqState::Done(text)) => break Ok(text.clone()),
                Some(ReqState::Failed(error)) => break Err(error.clone()),
                None => break Err("Transcription was cancelled.".into()),
                Some(ReqState::InFlight) => {}
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        };
        self.summary_voice.set(None);
        result
    }
}
