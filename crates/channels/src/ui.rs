//! The web pages, compiled into the service. They load nothing from
//! elsewhere: one HTML file each, with its own style and script.

pub const INDEX: &str = include_str!("../ui/index.html");
pub const ADVISER: &str = include_str!("../ui/adviser.html");
pub const EMPLOYER: &str = include_str!("../ui/employer.html");
pub const STAFF: &str = include_str!("../ui/staff.html");
pub const OPS: &str = include_str!("../ui/ops.html");
pub const STATUS: &str = include_str!("../ui/status.html");
