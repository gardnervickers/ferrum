//! A light model of the mill for the backplot: the table (fixed under the
//! toolpath) and the column, head, spindle and tool (moving with the tool),
//! as a few boxes in work coordinates. Defaults are a PM-728VT.

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
    /// Spindle axis to the column's front face (+Y side).
    pub throat: f64,
    /// Column width (X), depth (Y) and height above the table top.
    pub column: [f64; 3],
    /// Head width (X), height, and how far it reaches in front of the
    /// spindle axis (-Y).
    pub head: [f64; 3],
    /// Spindle nose diameter and length below the head.
    pub nose: [f64; 2],
    /// Motor diameter and height on top of the head.
    pub motor: [f64; 2],
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
        Machine {
            table: [800.0, 180.0, 55.0],
            table_center: [215.0, -90.0],
            slots: 3,
            nose_above_table: 380.0,
            throat: 230.0,
            column: [250.0, 200.0, 760.0],
            head: [240.0, 300.0, 90.0],
            nose: [70.0, 45.0],
            motor: [150.0, 200.0],
        }
    }
}

/// Where the machine is: the tool tip in work coordinates, the tool, and the
/// active work offset (machine = work + offset).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Pose {
    pub tip: P4,
    pub tool_len: f64,
    pub tool_dia: f64,
    pub offset: [f64; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layer {
    /// Under the toolpath (the table).
    Under,
    /// Over it, see-through (the moving parts).
    Over,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solid {
    pub lo: [f64; 3],
    pub hi: [f64; 3],
    pub rgb: [u8; 3],
    pub layer: Layer,
}

/// The T-slot lines on the table top, as (from, to) in work coordinates.
pub type Line = ([f64; 3], [f64; 3]);

impl Machine {
    pub fn scene(&self, pose: &Pose) -> (Vec<Solid>, Vec<Line>) {
        let o = pose.offset;
        let top = -self.nose_above_table - o[2];
        let (cx, cy) = (self.table_center[0] - o[0], self.table_center[1] - o[1]);
        let [tl, td, tt] = self.table;
        let mut solids = vec![Solid {
            lo: [cx - tl / 2.0, cy - td / 2.0, top - tt],
            hi: [cx + tl / 2.0, cy + td / 2.0, top],
            rgb: [0x2c, 0x33, 0x39],
            layer: Layer::Under,
        }];
        let lines = (0..self.slots)
            .map(|i| {
                let y = cy - td / 2.0 + td * (i as f64 + 1.0) / (self.slots as f64 + 1.0);
                ([cx - tl / 2.0, y, top], [cx + tl / 2.0, y, top])
            })
            .collect();
        let [x, y, z] = [pose.tip[0], pose.tip[1], pose.tip[2]];
        let len = pose.tool_len.max(0.0);
        let r = (pose.tool_dia / 2.0).max(1.5);
        let nose_z = z + len;
        let over = |lo, hi, rgb| Solid { lo, hi, rgb, layer: Layer::Over };
        // Tool, spindle nose, head, motor.
        if len > 0.5 {
            solids.push(over([x - r, y - r, z], [x + r, y + r, nose_z], [0xf2, 0xc1, 0x4e]));
        }
        let [nd, nl] = self.nose;
        solids.push(over([x - nd / 2.0, y - nd / 2.0, nose_z], [x + nd / 2.0, y + nd / 2.0, nose_z + nl], [0x9a, 0xa4, 0xad]));
        let [hw, hh, hf] = self.head;
        let head_lo = nose_z + nl;
        solids.push(over([x - hw / 2.0, y - hf, head_lo], [x + hw / 2.0, y + self.throat, head_lo + hh], [0x8c, 0x96, 0xa8]));
        let [md, mh] = self.motor;
        solids.push(over([x - md / 2.0, y - md / 2.0, head_lo + hh], [x + md / 2.0, y + md / 2.0, head_lo + hh + mh], [0x8c, 0x96, 0xa8]));
        // Column: moves with the spindle in X and Y (the table moves on the
        // real machine), fixed in Z.
        let [cw, cd, ch] = self.column;
        solids.push(over([x - cw / 2.0, y + self.throat, top - 250.0], [x + cw / 2.0, y + self.throat + cd, top + ch], [0x8c, 0x96, 0xa8]));
        (solids, lines)
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
        let (s, lines) = m.scene(&pose);
        let table = s.iter().find(|s| s.layer == Layer::Under).unwrap();
        // Nose 380 above the table at Z 0, so the table top is 180 below work Z 0.
        assert_eq!(table.hi[2], -180.0);
        assert_eq!((table.lo[0] + table.hi[0]) / 2.0, 0.0);
        assert_eq!(lines.len(), 3);
        let tool = s.iter().find(|s| s.rgb == [0xf2, 0xc1, 0x4e]).unwrap();
        assert_eq!((tool.lo[2], tool.hi[2]), (0.0, 80.0));
    }
}
