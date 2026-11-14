//! GamePad motion: accelerometer, gyroscope and attitude as `VPADStatus` reports them.
//!
//! The host has no GamePad to hold, so the front end gives a pose instead (how far the
//! GamePad is tipped forward, leaned sideways and turned) and [`TiltMotion`] works out what
//! the sensors would read: gravity seen from the tilted GamePad, the turning speed between
//! samples, the angle turned so far and the GamePad's axes.
//!
//! Axes are the GamePad's: X to the right, Y toward its top edge, Z out of the screen.
//! Lying flat with the screen up, the axes match the world's, whose Z points up.

/// One motion sample in `VPADStatus` units.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Motion {
    /// Acceleration, 1.0 = 1 g.
    pub acc: [f32; 3],
    /// Length of `acc`.
    pub acc_magnitude: f32,
    /// Length of the change of `acc` since the previous sample.
    pub acc_variation: f32,
    /// How flat (`x`, 1 lying flat) and how upright (`y`, 1 screen upright) the GamePad is.
    pub acc_vertical: [f32; 2],
    /// Angular velocity around X, Y and Z, 1.0 = 360° per second.
    pub gyro: [f32; 3],
    /// Angles turned so far, 1.0 = 360°.
    pub angle: [f32; 3],
    /// The GamePad's X, Y and Z axes in the world.
    pub direction: [[f32; 3]; 3],
}

impl Motion {
    /// No motion source: the GamePad lies still and flat.
    pub const REST: Self = Self {
        acc: [0.0, 0.0, -1.0],
        acc_magnitude: 1.0,
        acc_variation: 0.0,
        acc_vertical: [1.0, 0.0],
        gyro: [0.0; 3],
        angle: [0.0; 3],
        direction: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };
}

/// A pose of the GamePad, in degrees.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pose {
    /// Top edge tipped up (positive) or down, around X.
    pub pitch: f32,
    /// Leaned to the right (positive) or left, around Y.
    pub roll: f32,
    /// Turned to the left (positive) or right, around Z.
    pub yaw: f32,
}

type Matrix = [[f32; 3]; 3];

fn multiply(a: Matrix, b: Matrix) -> Matrix {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| (0..3).map(|k| a[row][k] * b[k][column]).sum())
    })
}

/// The rotation of `pose`: turned, then tipped, then leaned.
fn rotation(pose: Pose) -> Matrix {
    let (sp, cp) = pose.pitch.to_radians().sin_cos();
    let (sr, cr) = pose.roll.to_radians().sin_cos();
    let (sy, cy) = pose.yaw.to_radians().sin_cos();
    let yaw = [[cy, -sy, 0.0], [sy, cy, 0.0], [0.0, 0.0, 1.0]];
    let pitch = [[1.0, 0.0, 0.0], [0.0, cp, -sp], [0.0, sp, cp]];
    let roll = [[cr, 0.0, sr], [0.0, 1.0, 0.0], [-sr, 0.0, cr]];
    multiply(multiply(yaw, pitch), roll)
}

/// Turns sensor readings out of a sequence of poses.
#[derive(Debug, Clone, Copy, Default)]
pub struct TiltMotion {
    last: Option<(Pose, [f32; 3])>,
    angle: [f32; 3],
}

impl TiltMotion {
    /// The sensors for the GamePad held at `pose`, `step` seconds after the previous sample.
    pub fn update(&mut self, pose: Pose, step: f32) -> Motion {
        let r = rotation(pose);
        // Columns of the rotation are the GamePad's axes in the world.
        let direction = std::array::from_fn(|axis| std::array::from_fn(|i| r[i][axis]));
        // Gravity points down the world's Z; the GamePad sees it through its own axes.
        let acc = [-r[2][0], -r[2][1], -r[2][2]];
        let turned = [pose.pitch, pose.roll, pose.yaw];
        let (change, variation) = self.last.map_or(([0.0; 3], 0.0), |(last, last_acc)| {
            let before = [last.pitch, last.roll, last.yaw];
            let change = std::array::from_fn(|i| (turned[i] - before[i]) / 360.0);
            let moved = (0..3).map(|i| (acc[i] - last_acc[i]).powi(2)).sum::<f32>().sqrt();
            (change, moved)
        });
        self.last = Some((pose, acc));
        self.angle = std::array::from_fn(|i| self.angle[i] + change[i]);
        let gyro = if step > 0.0 { change.map(|turns| turns / step) } else { [0.0; 3] };
        Motion {
            acc,
            acc_magnitude: 1.0,
            acc_variation: variation,
            acc_vertical: [(-acc[2]).clamp(0.0, 1.0), (-acc[1]).clamp(-1.0, 1.0)],
            gyro,
            angle: self.angle,
            direction,
        }
    }

    /// The source stopped: the next pose starts without a jump in speed.
    pub fn release(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Intent: the level pose reads exactly like `Motion::REST`.
    /// Pass: every field of the first level sample equals REST's.
    #[test]
    fn level_is_rest() {
        let motion = TiltMotion::default().update(Pose::default(), 1.0 / 60.0);
        assert_eq!(motion, Motion::REST);
    }

    /// Intent: tipping the top edge up 90° stands the GamePad upright, so gravity runs
    /// down its Y axis and it reads as upright, not flat.
    /// Pass: acc ≈ (0, -1, 0), vertical ≈ (0, 1), the Y axis points up.
    #[test]
    fn tipped_up_is_upright() {
        let pose = Pose { pitch: 90.0, ..Pose::default() };
        let motion = TiltMotion::default().update(pose, 1.0 / 60.0);
        assert!(close(motion.acc[1], -1.0) && close(motion.acc[2], 0.0), "{:?}", motion.acc);
        assert!(close(motion.acc_vertical[0], 0.0) && close(motion.acc_vertical[1], 1.0));
        assert!(close(motion.direction[1][2], 1.0), "{:?}", motion.direction);
    }

    /// Intent: leaning right moves gravity toward the GamePad's right side.
    /// Pass: 30° of roll gives acc.x = sin 30°.
    #[test]
    fn leaning_moves_gravity_sideways() {
        let pose = Pose { roll: 30.0, ..Pose::default() };
        let motion = TiltMotion::default().update(pose, 1.0 / 60.0);
        assert!(close(motion.acc[0], 0.5), "{:?}", motion.acc);
    }

    /// Intent: turning between samples is angular velocity in turns per second, adds up
    /// in `angle`, and a released source does not report a jump when it starts again.
    /// Pass: 6° in 1/60 s is 1 turn/s; angle sums the moves; after release the speed is 0.
    #[test]
    fn turning_speed_and_angle() {
        let mut tilt = TiltMotion::default();
        tilt.update(Pose::default(), 1.0 / 60.0);
        let motion = tilt.update(Pose { yaw: 6.0, ..Pose::default() }, 1.0 / 60.0);
        assert!(close(motion.gyro[2], 1.0), "{:?}", motion.gyro);
        let motion = tilt.update(Pose { yaw: 12.0, ..Pose::default() }, 1.0 / 60.0);
        assert!(close(motion.angle[2], 12.0 / 360.0));
        tilt.release();
        assert_eq!(tilt.update(Pose { yaw: 90.0, ..Pose::default() }, 0.01).gyro, [0.0; 3]);
    }
}
