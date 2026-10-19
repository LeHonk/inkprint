use alloc::collections::VecDeque;
use alloc::string::String;

/// `job-state` enum values (RFC 8011, 5.3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JobState {
    Pending = 3,
    Processing = 5,
    Canceled = 7,
    Aborted = 8,
    Completed = 9,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Canceled | JobState::Aborted | JobState::Completed)
    }

    pub fn reason(self) -> &'static str {
        match self {
            JobState::Pending => "job-incoming",
            JobState::Processing => "job-printing",
            JobState::Canceled => "job-canceled-by-user",
            JobState::Aborted => "aborted-by-system",
            JobState::Completed => "job-completed-successfully",
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: u32,
    pub name: String,
    pub state: JobState,
    pub pages: u32,
    /// Seconds since boot.
    pub created: u32,
}

/// The most recent jobs, oldest dropped first.
pub struct JobTable {
    jobs: VecDeque<JobRecord>,
}

impl JobTable {
    pub const CAPACITY: usize = 10;

    pub fn new() -> Self {
        Self { jobs: VecDeque::with_capacity(Self::CAPACITY) }
    }

    pub fn push(&mut self, job: JobRecord) {
        if self.jobs.len() == Self::CAPACITY {
            self.jobs.pop_front();
        }
        self.jobs.push_back(job);
    }

    pub fn get(&self, id: u32) -> Option<&JobRecord> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn update(&mut self, id: u32, f: impl FnOnce(&mut JobRecord)) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            f(job);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &JobRecord> {
        self.jobs.iter()
    }

    pub fn active_count(&self) -> usize {
        self.jobs.iter().filter(|j| !j.state.is_terminal()).count()
    }
}

impl Default for JobTable {
    fn default() -> Self {
        Self::new()
    }
}
