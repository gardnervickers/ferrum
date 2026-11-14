//! A light model of the mill for the backplot: the table (fixed under the
//! toolpath) and the tool in the spindle (moving with the tip), in work
//! coordinates. Defaults are a PM-728VT.

use crate::P4;

/// Machine dimensions in mm. Machine coordinates are LinuxCNC's: the
/// controlled point is the spindle nose, Z 0 is the top of travel.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    /// Table length (X), depth (Y) and thickness.
    pub table: [f64; 3],
    /// Machine X Y with the spindle over the middle of the table.
    pub table_center: [f64; 2],
    /// T-slots along X across the table top.
    pub slots: u32,
    /// Spindle nose above the table top at machine Z 0.
    pub nose_above_table: f64,
}

impl Default for Machine {
    fn default() -> Self {
        Self::pm728vt()
    }
}

impl Machine {
    /// Published PM-728VT sizes (table 800 x 180, 380 mm nose to table at
    /// the top of travel) with the travel of mymill.ini (X 0..430, Y -180..0).
    pub fn pm728vt() -> Self {
        Machine { table: [800.0, 180.0, 55.0], table_center: [215.0, -90.0], slots: 3, nose_above_table: 380.0 }
    }
}

/// Where the machine is: the tool tip in work coordinates, the loaded tool
/// (0 when unknown), and the active work offset (machine = work + offset).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Pose {
    pub tip: P4,
    pub tool_len: f64,
    pub tool_dia: f64,
    pub offset: [f64; 3],
}

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solid {
    pub lo: [f64; 3],
    pub hi: [f64; 3],
    pub rgb: [u8; 3],
}

/// A vertical cylinder: the tool, from its tip up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cylinder {
    pub center: [f64; 2],
    pub z0: f64,
    pub z1: f64,
    pub r: f64,
}

/// A line as (from, to) in work coordinates.
pub type Line = ([f64; 3], [f64; 3]);

#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub table: Solid,
    /// The T-slots on the table top.
    pub slots: Vec<Line>,
    pub tool: Option<Cylinder>,
}

impl Machine {
    pub fn scene(&self, pose: &Pose) -> Scene {
        let o = pose.offset;
        let top = -self.nose_above_table - o[2];
        let (cx, cy) = (self.table_center[0] - o[0], self.table_center[1] - o[1]);
        let [tl, td, tt] = self.table;
        let table = Solid {
            lo: [cx - tl / 2.0, cy - td / 2.0, top - tt],
            hi: [cx + tl / 2.0, cy + td / 2.0, top],
            rgb: [0x2c, 0x33, 0x39],
        };
        let slots = (0..self.slots)
            .map(|i| {
                let y = cy - td / 2.0 + td * (i as f64 + 1.0) / (self.slots as f64 + 1.0);
                ([cx - tl / 2.0, y, top], [cx + tl / 2.0, y, top])
            })
            .collect();
        // The tool at its table diameter. Its length is the table length
        // when known, else a stub four diameters long.
        let tool = (pose.tool_dia > 0.0).then(|| {
            let r = pose.tool_dia / 2.0;
            let len = if pose.tool_len > 0.5 { pose.tool_len } else { (4.0 * pose.tool_dia).clamp(10.0, 40.0) };
            Cylinder { center: [pose.tip[0], pose.tip[1]], z0: pose.tip[2], z1: pose.tip[2] + len, r }
        });
        Scene { table, slots, tool }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_top_is_where_the_tip_would_touch_it() {
        let m = Machine::pm728vt();
        // Work Z 0 is 200 mm below the top of travel (machine Z -200).
        let pose = Pose { tip: [0.0, 0.0, 0.0, 0.0], tool_len: 80.0, tool_dia: 10.0, offset: [215.0, -90.0, -200.0] };
        let s = m.scene(&pose);
        // Nose 380 above the table at Z 0, so the table top is 180 below work Z 0.
        assert_eq!(s.table.hi[2], -180.0);
        assert_eq!((s.table.lo[0] + s.table.hi[0]) / 2.0, 0.0);
        assert_eq!(s.slots.len(), 3);
        let t = s.tool.unwrap();
        assert_eq!((t.z0, t.z1, t.r), (0.0, 80.0, 5.0));
    }

    #[test]
    fn unknown_tool_is_not_drawn_and_unknown_length_is_a_stub() {
        let m = Machine::pm728vt();
        let mut pose = Pose { tool_dia: 0.0, ..Default::default() };
        assert!(m.scene(&pose).tool.is_none());
        pose.tool_dia = 6.0;
        assert_eq!(m.scene(&pose).tool.unwrap().z1, 24.0);
    }
}
