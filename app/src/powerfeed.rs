//! Power feed and GO TO, with parity to Probe Basic's powerfeed tab
//! (custom_ui/tabs/powerfeed/powerfeed.py). Moves run through
//! `o<mymill_powerfeed>`, which checks its arguments, selects the power feed
//! WCS only for the move and restores modal state (M73) when it returns.
//! The checks here are a preflight for clear messages; LinuxCNC and HAL
//! still decide whether motion happens.

use crate::config::Config;
use crate::settings::Settings;
use machine::{Interp, Status};

pub const WCS: [&str; 9] = ["G54", "G55", "G56", "G57", "G58", "G59", "G59.1", "G59.2", "G59.3"];
/// End of travel stops this far inside the soft limit.
pub const CLEARANCE: f64 = 1.0;
const TOL: f64 = 0.0001;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Distance,
    EndOfTravel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Goto {
    pub name: String,
    pub x: f64,
    pub y: f64,
}

pub struct PowerFeed {
    pub mode: Mode,
    pub feed: f64,
    pub dist: f64,
    /// 1-based WCS index (1 = G54), as `powerfeed.wcs-index`.
    pub wcs: usize,
    pub gt_x: f64,
    pub gt_y: f64,
    pub gt_feed: f64,
    pub gotos: Vec<Goto>,
    pub selected: Option<usize>,
    /// Machine soft limits for X, Y, Z from [AXIS_*]MIN/MAX_LIMIT.
    limits: [Option<(f64, f64)>; 3],
}

/// What a move needs before it is offered. Mirrors powerfeed.py's
/// `_poll_machine_state`, with the drive check coming from the modules.
pub fn ready(s: &Status, motion_block: &str) -> Result<(), String> {
    if !s.power_on() {
        return Err("Machine must be ON".into());
    }
    if s.interp != Interp::Idle {
        return Err("Interpreter busy".into());
    }
    if !s.homed.iter().take(3).all(|h| *h) {
        return Err("Home all joints".into());
    }
    if !motion_block.is_empty() {
        return Err(motion_block.into());
    }
    Ok(())
}

fn call(op: u8, wcs: usize, axis: usize, value: f64, feed: f64, y: f64) -> String {
    format!("o<mymill_powerfeed> call [{op}] [{wcs}] [{axis}] [{value:.4}] [{feed:.3}] [{y:.4}]")
}

impl PowerFeed {
    pub fn new(cfg: &Config, st: &Settings) -> Self {
        let limit = |a: &str| {
            let lo = cfg.get_f64(&format!("AXIS_{a}"), "MIN_LIMIT")?;
            let hi = cfg.get_f64(&format!("AXIS_{a}"), "MAX_LIMIT")?;
            (hi - lo > 2.0 * CLEARANCE).then_some((lo, hi))
        };
        let gotos = st
            .list("powerfeed.goto.")
            .into_iter()
            .filter_map(|v| {
                let mut p = v.split('|');
                let name = p.next()?.to_string();
                Some(Goto { name, x: p.next()?.parse().ok()?, y: p.next()?.parse().ok()? })
            })
            .collect();
        let wcs = st.f64("powerfeed.wcs-index", 1.0) as usize;
        Self {
            mode: if st.get("powerfeed.xy-mode") == Some("end-of-travel") { Mode::EndOfTravel } else { Mode::Distance },
            feed: st.f64("powerfeed.feed", 250.0),
            dist: st.f64("powerfeed.distance", 25.0),
            wcs: if (1..=WCS.len()).contains(&wcs) { wcs } else { 1 },
            gt_x: 0.0,
            gt_y: 0.0,
            gt_feed: st.f64("powerfeed.feed", 250.0),
            gotos,
            selected: None,
            limits: [limit("X"), limit("Y"), limit("Z")],
        }
    }

    pub fn save(&self, st: &mut Settings) {
        st.set("powerfeed.feed", self.feed);
        st.set("powerfeed.distance", self.dist);
        st.set("powerfeed.wcs-index", self.wcs);
        st.set("powerfeed.xy-mode", if self.mode == Mode::EndOfTravel { "end-of-travel" } else { "distance" });
        st.set_list("powerfeed.goto.", self.gotos.iter().map(|g| format!("{}|{}|{}", g.name, g.x, g.y)));
    }

    pub fn wcs_code(&self) -> &'static str {
        WCS[self.wcs - 1]
    }

    /// Z only moves by distance; end of travel is for X and Y.
    pub fn axis_enabled(&self, axis: usize) -> bool {
        match self.mode {
            Mode::Distance => self.dist > 0.0,
            Mode::EndOfTravel => axis < 2,
        }
    }

    /// The MDI line for a power feed move, and what to tell the operator.
    pub fn move_cmd(&self, s: &Status, axis: usize, dir: i32) -> Result<(String, String), String> {
        let name = format!("{}{}", ['X', 'Y', 'Z'][axis], if dir > 0 { '+' } else { '-' });
        if self.feed <= 0.0 {
            return Err("Feed must be greater than zero".into());
        }
        if self.mode == Mode::EndOfTravel {
            if axis > 1 {
                return Err("Z moves by distance only".into());
            }
            let (lo, hi) = self.limits[axis].ok_or(format!("{} travel limits unavailable", &name[..1]))?;
            let target = if dir > 0 { hi - CLEARANCE } else { lo + CLEARANCE };
            let pos = s.actual[axis];
            if (dir > 0 && pos >= target - TOL) || (dir < 0 && pos <= target + TOL) {
                return Err(format!("{name} already at end of travel"));
            }
            return Ok((call(2, 0, axis + 1, target, self.feed, 0.0), format!("Running {name} to end of travel")));
        }
        if self.dist <= 0.0 {
            return Err("Distance is required".into());
        }
        let d = self.dist * dir as f64;
        Ok((call(1, self.wcs, axis + 1, d, self.feed, 0.0), format!("Running {name} {:.3} at {:.0} mm/min", self.dist, self.feed)))
    }

    pub fn goto_cmd(&self) -> Result<(String, String), String> {
        if self.gt_feed <= 0.0 {
            return Err("GO TO feed must be greater than zero".into());
        }
        Ok((
            call(3, self.wcs, 0, self.gt_x, self.gt_feed, self.gt_y),
            format!("GO TO {} X{:.3} Y{:.3} at F{:.0}", self.wcs_code(), self.gt_x, self.gt_y, self.gt_feed),
        ))
    }

    /// Retract Z in the active WCS, then XY to its zero.
    pub fn retract_cmd(&self, s: &Status, retract: f64) -> Result<String, String> {
        if retract <= 0.0 {
            return Err("Z retract must be greater than zero".into());
        }
        if let Some((_, hi)) = self.limits[2] {
            if s.actual[2] + retract > hi - TOL {
                return Err("Z retract would exceed machine limit".into());
            }
        }
        Ok(call(4, 0, 0, retract, 0.0, 0.0))
    }

    pub fn z_zero_cmd(&self) -> String {
        call(5, 0, 0, 0.0, 0.0, 0.0)
    }

    pub fn select(&mut self, i: usize) {
        if let Some(g) = self.gotos.get(i) {
            self.selected = Some(i);
            self.gt_x = g.x;
            self.gt_y = g.y;
        }
    }

    pub fn save_goto(&mut self) {
        let used: Vec<&str> = self.gotos.iter().map(|g| g.name.as_str()).collect();
        let mut n = self.gotos.len() + 1;
        while used.contains(&format!("P{n}").as_str()) {
            n += 1;
        }
        self.gotos.push(Goto { name: format!("P{n}"), x: self.gt_x, y: self.gt_y });
        self.selected = Some(self.gotos.len() - 1);
    }

    pub fn update_goto(&mut self) {
        if let Some(g) = self.selected.and_then(|i| self.gotos.get_mut(i)) {
            g.x = self.gt_x;
            g.y = self.gt_y;
        }
    }

    pub fn delete_goto(&mut self) {
        if let Some(i) = self.selected.filter(|&i| i < self.gotos.len()) {
            self.gotos.remove(i);
            self.selected = if self.gotos.is_empty() { None } else { Some(i.min(self.gotos.len() - 1)) };
        }
    }

    /// Preview positions in 0..1 (y up) for the saved points, the target
    /// and the WCS origin, framed together with a margin.
    pub fn preview(&self) -> (Vec<(f32, f32)>, (f32, f32), (f32, f32)) {
        let pts: Vec<(f64, f64)> = self.gotos.iter().map(|g| (g.x, g.y)).collect();
        let all = pts.iter().copied().chain([(self.gt_x, self.gt_y), (0.0, 0.0)]);
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for (x, y) in all {
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let span = (x1 - x0).max(y1 - y0).max(20.0) * 1.2;
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let map = |(x, y): (f64, f64)| ((0.5 + (x - cx) / span) as f32, (0.5 - (y - cy) / span) as f32);
        (pts.into_iter().map(map).collect(), map((self.gt_x, self.gt_y)), map((0.0, 0.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf() -> PowerFeed {
        let mut cfg = Config::default();
        cfg.insert("AXIS_X", "MIN_LIMIT", "-200");
        cfg.insert("AXIS_X", "MAX_LIMIT", "200");
        PowerFeed::new(&cfg, &Settings::memory())
    }

    #[test]
    fn distance_pass_uses_the_selected_wcs() {
        let mut p = pf();
        p.wcs = 2;
        let (cmd, _) = p.move_cmd(&Status::default(), 1, -1).unwrap();
        assert_eq!(cmd, "o<mymill_powerfeed> call [1] [2] [2] [-25.0000] [250.000] [0.0000]");
    }

    #[test]
    fn end_of_travel_stops_short_and_never_moves_z() {
        let mut p = pf();
        p.mode = Mode::EndOfTravel;
        let (cmd, _) = p.move_cmd(&Status::default(), 0, 1).unwrap();
        assert!(cmd.starts_with("o<mymill_powerfeed> call [2] [0] [1] [199.0000]"));
        assert!(p.move_cmd(&Status::default(), 2, 1).is_err());
        assert!(!p.axis_enabled(2));
        let mut s = Status::default();
        s.actual[0] = 199.0;
        assert!(p.move_cmd(&s, 0, 1).is_err());
        assert!(p.move_cmd(&Status::default(), 1, 1).is_err(), "no Y limits configured");
    }

    #[test]
    fn goto_list_names_and_edits() {
        let mut p = pf();
        p.gt_x = 5.0;
        p.save_goto();
        p.save_goto();
        assert_eq!(p.gotos.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["P1", "P2"]);
        p.select(0);
        p.gt_y = 3.0;
        p.update_goto();
        assert_eq!(p.gotos[0], Goto { name: "P1".into(), x: 5.0, y: 3.0 });
        p.delete_goto();
        assert_eq!(p.gotos.len(), 1);
        assert_eq!(p.selected, Some(0));
    }
}
