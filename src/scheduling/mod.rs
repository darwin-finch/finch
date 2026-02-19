// Autonomous task scheduling system
//
// Enables the AI to schedule its own tasks and resume work without human intervention

pub mod queue;
pub mod scheduler;

pub use queue::{ScheduledTask, TaskQueue, TaskStatus};
pub use scheduler::TaskScheduler;
