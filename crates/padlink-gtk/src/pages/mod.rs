//! The per-controller pages of the app.

mod bridge;
mod doctor;
mod live;
mod logs;
mod overview;
mod probe;
mod rumble;

use std::rc::Rc;

use adw::gtk;
use padlink_core::discovery::Controller;

use crate::util::Toast;

/// One tab. Pages receive the selected controller (or None) and a toast callback.
pub trait Page {
    fn name(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn icon(&self) -> &'static str;
    fn widget(&self) -> gtk::Widget;
    fn set_controller(&self, ctrl: Option<&Controller>);
    fn shutdown(&self) {}
    /// A running bridge stays attached to its controller while another is selected.
    fn busy(&self) -> bool {
        false
    }
    /// Don't interrupt a running session on a mere hotplug refresh of the same controller.
    fn keep_on_refresh(&self) -> bool {
        false
    }
}

pub fn all(toast: &Toast) -> Vec<Rc<dyn Page>> {
    vec![
        overview::OverviewPage::new(toast.clone()),
        live::LivePage::new(toast.clone()),
        rumble::RumblePage::new(toast.clone()),
        probe::ProbePage::new(toast.clone()),
        bridge::BridgePage::new(toast.clone()),
        doctor::DoctorPage::new(toast.clone()),
        logs::LogsPage::new(toast.clone()),
    ]
}
