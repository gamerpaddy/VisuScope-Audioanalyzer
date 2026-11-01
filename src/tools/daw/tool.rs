//! The `Tool` implementation: per-frame work, status bar, settings and commands.

use super::*;

impl Tool for Daw {
    fn update(&mut self, cx: &mut Ctx, visible: bool) {
        self.visible = visible;
        if self.startup == 0 {
            self.startup = 1;
            if std::env::var_os("VISUSCOPE_DAW_DEMO").is_some() {
                self.demo();
            } else if autosave_path().exists() {
                // bring back the last session (recordings are also kept as WAV files in Documents)
                self.open_project(autosave_path(), true);
            }
        }
        self.poll(cx);
        self.manage_capture();
        self.sync_play(cx);
        self.manage_monitor(cx);
        self.update_meters();
        if self.rev != self.saved_rev && self.last_edit.elapsed() > Duration::from_secs(4) && self.busy.load(Relaxed) == 0 && self.rec.is_none() {
            self.save_project(autosave_path(), true);
        }
        if self.play.is_some() || self.rec.is_some() {
            // the transport display must run even when nothing else repaints
        }
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Ctx) {
        self.draw(ui, cx);
    }

    fn status(&self) -> Option<(String, Color32)> {
        if let Some(r) = &self.rec {
            return Some((format!("REC {}", ui::clock(r.t0.elapsed().as_secs_f32())), Color32::from_rgb(255, 100, 100)));
        }
        if self.play.is_some() {
            return Some((format!("Timeline {}", ui::clock(self.pos() as f32 / self.sr() as f32)), Color32::from_rgb(120, 190, 255)));
        }
        None
    }

    fn save(&self, o: &mut SettingsOut) {
        o.put("snap", self.snap);
        o.put("follow", self.follow);
        o.put("loop", self.loop_on);
        o.put("analysis", self.an_open);
        o.put("an_track", self.an_src == AnSrc::Track);
        o.put("sound", self.sound);
        o.put("feed_an", self.feed_analysers);
        o.put("mon_dev", &self.mon_dev);
        o.put("mon_vol", self.mon_vol);
        o.put("rec_off", self.rec_off_ms);
        o.put("pps", self.pps);
    }

    fn load(&mut self, i: &SettingsIn) {
        i.set("snap", &mut self.snap);
        i.set("follow", &mut self.follow);
        i.set("loop", &mut self.loop_on);
        i.set("analysis", &mut self.an_open);
        i.set("sound", &mut self.sound);
        i.set("feed_an", &mut self.feed_analysers);
        i.set("mon_vol", &mut self.mon_vol);
        self.mon_vol = self.mon_vol.clamp(0.0, 1.0);
        if let Some(t) = i.get::<bool>("an_track") {
            self.an_src = if t { AnSrc::Track } else { AnSrc::Master };
        }
        if let Some(d) = i.str("mon_dev") {
            self.mon_dev = d;
        }
        if let Some(v) = i.get::<f32>("rec_off") {
            self.rec_off_ms = v.clamp(-500.0, 500.0);
        }
        if let Some(v) = i.get::<f64>("pps") {
            if v.is_finite() {
                self.pps = v.clamp(0.5, 60000.0);
            }
        }
    }

    fn command(&mut self, c: &str) {
        let mut it = c.splitn(2, '\t');
        match (it.next(), it.next()) {
            (Some("import"), Some(path)) => self.import(vec![PathBuf::from(path)]),
            (Some("demo"), _) => self.demo(),
            _ => {}
        }
    }
}
