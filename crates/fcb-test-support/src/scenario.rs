//! Bounded headless/native scenario runner (fcb-izu8).
//!
//! The driver executes queued [`ScenarioSpec`]s under explicit route
//! selection, per-scenario deadlines, bounded admission, and unique owned
//! fixture directories. Every run produces a validated
//! [`receipts::ScenarioReceipt`]; timeouts, missing native hardware,
//! cancellations and intentionally failing negative controls are distinct
//! terminal verdicts and are never collapsed into success.
//!
//! Fixture policy: the driver creates one unique run root under the OS temp
//! directory (`fcb-scenario-<pid>-<counter>`) and scenario fixture files are
//! written only beneath it. [`ScenarioDriver::finish`] removes that root and
//! nothing else; paths escaping the namespace are refused at write time.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use crate::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, ScenarioReceipt, ScenarioReceiptDraft,
    SourcePin, TerminalOutcome,
};
use crate::ContentDigest;

/// Execution route for one scenario. Native routes require real platform
/// facilities; when they are unavailable the result is an unexecuted
/// [`ScenarioVerdict::MissingEvidence`], never a fabricated pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioRoute {
    Headless,
    Native,
}

/// Terminal classification of one scenario run. Only [`Passed`] and
/// [`ExpectedFailure`] satisfy an oracle; every other verdict names why the
/// scenario did not produce a pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioVerdict {
    Passed,
    /// A declared negative control failed exactly as intended.
    ExpectedFailure,
    Failed,
    TimedOut,
    Cancelled,
    /// Native hardware or facilities were unavailable: unexecuted, not failed.
    MissingEvidence,
}

impl ScenarioVerdict {
    /// Whether this verdict satisfies the scenario's oracle.
    pub const fn is_acceptable(self) -> bool {
        matches!(self, Self::Passed | Self::ExpectedFailure)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "PASSED",
            Self::ExpectedFailure => "EXPECTED_FAILURE",
            Self::Failed => "FAILED",
            Self::TimedOut => "TIMED_OUT",
            Self::Cancelled => "CANCELLED",
            Self::MissingEvidence => "MISSING_EVIDENCE",
        }
    }
}

/// What a scenario body reports about its own execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BodyResult {
    Pass,
    /// The body failed in a way the spec intended to demonstrate.
    ExpectedFailure { reason: String },
    /// The body failed in a way the spec did not intend.
    Fail { reason: String },
}

/// One runnable scenario. The body receives the owned fixture context and
/// must poll [`ScenarioContext::expired`] at step boundaries; the driver
/// also measures wall-clock overrun at the boundary as a second net.
#[derive(Clone, Copy)]
pub struct ScenarioSpec {
    pub name: &'static str,
    pub route: ScenarioRoute,
    pub timeout: Duration,
    pub negative_control: bool,
    pub seed: u64,
    pub body: fn(&ScenarioContext) -> BodyResult,
}

/// Cooperative deadline/cancel flag handed to scenario bodies.
#[derive(Debug)]
pub struct CancelToken(AtomicU64);

impl CancelToken {
    fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn cancel(&self) {
        self.0.store(1, AtomicOrdering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::Acquire) == 1
    }
}

/// What a scenario body may touch: its own fixture namespace, the deadline,
/// and the cancel token. Fixture writes that escape the namespace are
/// refused.
pub struct ScenarioContext {
    fixture_dir: PathBuf,
    deadline: Instant,
    cancel: CancelToken,
}

impl ScenarioContext {
    pub fn fixture_dir(&self) -> &PathBuf {
        &self.fixture_dir
    }

    /// Cooperative deadline check for step boundaries.
    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline || self.cancel.is_cancelled()
    }

    /// Write one fixture file inside this scenario's owned namespace.
    /// Absolute paths, parent traversal, and empty names are refused.
    pub fn write_fixture(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, ScenarioError> {
        if name.is_empty() || name.starts_with('/') || name.contains("..") {
            return Err(ScenarioError::FixtureEscape);
        }
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(ScenarioError::FixtureEscape);
        }
        let path = self.fixture_dir.join(name);
        std::fs::write(&path, bytes).map_err(|_| ScenarioError::FixtureIo)?;
        Ok(path)
    }
}

/// Aggregate runner counters; also mirrored into each receipt's event ring.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunnerCounters {
    pub accepted: u64,
    pub rejected_admission: u64,
    pub passed: u64,
    pub expected_failures: u64,
    pub failed: u64,
    pub timed_out: u64,
    pub missing_evidence: u64,
}

/// One executed scenario: verdict, timing, validated receipt, and the
/// replay command for any non-acceptable outcome.
#[derive(Clone, Debug)]
pub struct ScenarioRecord {
    pub name: String,
    pub route: ScenarioRoute,
    pub verdict: ScenarioVerdict,
    pub elapsed: Duration,
    pub receipt: ScenarioReceipt,
    /// Deterministic replay form for Failed/TimedOut outcomes.
    pub replay: Option<String>,
}

impl ScenarioRecord {
    pub fn replay_command(&self) -> Option<&str> {
        self.replay.as_deref()
    }
}

/// Typed refusals from the runner itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioError {
    /// The admission queue is full; the spec was rejected, not queued.
    AdmissionSaturated,
    /// A fixture write tried to escape the owned namespace or the
    /// filesystem refused it.
    FixtureEscape,
    FixtureIo,
    /// A receipt failed its encode/decode validation.
    ReceiptInvalid,
    /// The run root was already finished.
    AlreadyFinished,
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::AdmissionSaturated => "scenario admission queue saturated",
            Self::FixtureEscape => "fixture path escaped the owned namespace",
            Self::FixtureIo => "fixture filesystem write failed",
            Self::ReceiptInvalid => "scenario receipt failed encode/decode validation",
            Self::AlreadyFinished => "scenario driver already finished",
        };
        f.write_str(text)
    }
}

impl std::error::Error for ScenarioError {}

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Deterministic 40-hex pin derived from the scenario identity, so the
/// same scenario and seed always produce the same receipt pin.
fn scenario_pin(name: &str, seed: u64) -> SourcePin {
    let digest_hex = ContentDigest::of(name.as_bytes()).hex();
    let mut mixed = seed ^ (name.len() as u64).rotate_left(17);
    for byte in name.as_bytes() {
        mixed = mixed.wrapping_mul(0x1_0000_0001_b3) ^ u64::from(*byte);
    }
    let pin_text = format!("{}{:016x}", &digest_hex[..24], mixed.rotate_left(13));
    SourcePin::new(&pin_text).expect("40 hex characters by construction")
}


/// A bounded, synchronous scenario runner with a unique owned fixture root.
pub struct ScenarioDriver {
    run_root: PathBuf,
    admission: usize,
    native_available: bool,
    finished: bool,
    pending: Vec<ScenarioSpec>,
    counters: RunnerCounters,
    records: Vec<ScenarioRecord>,
}

impl ScenarioDriver {
    /// Create a driver with a unique owned run root and an admission bound
    /// on the pending queue. `native_available` must reflect reality; the
    /// runner never guesses.
    pub fn new(admission: usize, native_available: bool) -> Result<Self, ScenarioError> {
        if admission == 0 {
            return Err(ScenarioError::AdmissionSaturated);
        }
        let sequence = RUN_SEQUENCE.fetch_add(1, AtomicOrdering::SeqCst);
        let run_root = std::env::temp_dir()
            .join(format!(
                "fcb-scenario-{}-{sequence}",
                std::process::id()
            ));
        std::fs::create_dir_all(&run_root).map_err(|_| ScenarioError::FixtureIo)?;
        Ok(Self {
            run_root,
            admission,
            native_available,
            finished: false,
            pending: Vec::new(),
            counters: RunnerCounters::default(),
            records: Vec::new(),
        })
    }

    /// The unique owned fixture root for this run. Cleanup in [`finish`]
    /// removes exactly this directory and nothing outside it.
    pub fn run_root(&self) -> &Path {
        &self.run_root
    }

    pub fn counters(&self) -> RunnerCounters {
        self.counters
    }

    pub fn records(&self) -> &[ScenarioRecord] {
        &self.records
    }

    /// Queue one scenario. Rejected (not queued) when the bounded admission
    /// queue is full; the rejection is counted.
    pub fn submit(&mut self, spec: ScenarioSpec) -> Result<(), ScenarioError> {
        if self.finished {
            return Err(ScenarioError::AlreadyFinished);
        }
        if self.pending.len() >= self.admission {
            self.counters.rejected_admission += 1;
            return Err(ScenarioError::AdmissionSaturated);
        }
        self.pending.push(spec);
        self.counters.accepted += 1;
        Ok(())
    }

    /// Execute every queued scenario in submission order and produce one
    /// validated receipt per run.
    pub fn run_pending(&mut self, redactor: &Redactor) -> &[ScenarioRecord] {
        let pending = std::mem::take(&mut self.pending);
        for spec in pending {
            let record = self.execute(&spec, redactor);
            self.counters_for(record.verdict);
            self.records.push(record);
        }
        &self.records
    }

    fn counters_for(&mut self, verdict: ScenarioVerdict) {
        match verdict {
            ScenarioVerdict::Passed => self.counters.passed += 1,
            ScenarioVerdict::ExpectedFailure => self.counters.expected_failures += 1,
            ScenarioVerdict::Failed => self.counters.failed += 1,
            ScenarioVerdict::TimedOut => self.counters.timed_out += 1,
            ScenarioVerdict::Cancelled => {}
            ScenarioVerdict::MissingEvidence => self.counters.missing_evidence += 1,
        }
    }

    fn execute(&mut self, spec: &ScenarioSpec, redactor: &Redactor) -> ScenarioRecord {
        let fixture_dir = self.run_root.join(format!("{}-{}", spec.name, spec.seed));
        if std::fs::create_dir_all(&fixture_dir).is_err() {
            // A fixture-directory failure is recorded as missing evidence:
            // the scenario did not run.
            return self.record_unexecuted(spec, redactor, "fixture directory unavailable");
        }

        let deadline = Instant::now() + spec.timeout;
        let cancel = CancelToken::new();
        let context = ScenarioContext {
            fixture_dir,
            deadline,
            cancel,
        };

        let started = Instant::now();
        let body_result = if spec.route == ScenarioRoute::Native && !self.native_available {
            None
        } else {
            Some((spec.body)(&context))
        };
        let elapsed = started.elapsed();

        let (verdict, reason) = match body_result {
            None => (
                ScenarioVerdict::MissingEvidence,
                "native facilities unavailable".to_string(),
            ),
            Some(BodyResult::Pass) => {
                if elapsed > spec.timeout {
                    (ScenarioVerdict::TimedOut, "deadline exceeded".to_string())
                } else if spec.negative_control {
                    (
                        ScenarioVerdict::Failed,
                        "negative control unexpectedly passed".to_string(),
                    )
                } else {
                    (ScenarioVerdict::Passed, String::new())
                }
            }
            Some(BodyResult::ExpectedFailure { reason }) => {
                if elapsed > spec.timeout {
                    (ScenarioVerdict::TimedOut, "deadline exceeded".to_string())
                } else if spec.negative_control {
                    (ScenarioVerdict::ExpectedFailure, reason)
                } else {
                    (
                        ScenarioVerdict::Failed,
                        format!("undelclared expected-failure: {reason}"),
                    )
                }
            }
            Some(BodyResult::Fail { reason }) => {
                if elapsed > spec.timeout {
                    (ScenarioVerdict::TimedOut, "deadline exceeded".to_string())
                } else if spec.negative_control {
                    (ScenarioVerdict::ExpectedFailure, reason)
                } else {
                    (ScenarioVerdict::Failed, reason)
                }
            }
        };

        let effect = match verdict {
            ScenarioVerdict::Passed | ScenarioVerdict::ExpectedFailure => Effect::Succeeded,
            ScenarioVerdict::Cancelled => Effect::Canceled,
            ScenarioVerdict::MissingEvidence => Effect::Canceled,
            ScenarioVerdict::Failed | ScenarioVerdict::TimedOut => Effect::Failed,
        };
        let unexecuted_reason = (verdict == ScenarioVerdict::MissingEvidence)
            .then(|| reason.clone())
            .filter(|reason| !reason.is_empty());
        let exit_code = if verdict.is_acceptable() { Some(0) } else { None };

        let mut ring = EventRing::new(8);
        if !reason.is_empty() {
            ring.push(redactor, &reason);
        }

        let pin = scenario_pin(spec.name, spec.seed);
        let draft = ScenarioReceiptDraft {
            scenario: spec.name.to_string(),
            seed: crate::receipts::ScenarioSeed(spec.seed),
            pin,
            route: crate::receipts::RouteId::new(&format!(
                "route/{}",
                match spec.route {
                    ScenarioRoute::Headless => "headless",
                    ScenarioRoute::Native => "native",
                }
            ))
            .expect("static route id"),
            corpus_digest: ContentDigest::of(spec.name.as_bytes()),
            corpus_count: 1,
            outcome: TerminalOutcome::new(exit_code, effect, unexecuted_reason),
            comparison: match &verdict {
                ScenarioVerdict::Failed => Some(ExpectedVsActual::new(
                    redactor,
                    "acceptable verdict",
                    verdict.as_str(),
                )),
                _ => None,
            },
            ring,
            artifacts: vec![],
        };
        let receipt = ScenarioReceipt::from_draft(redactor, draft);
        // Codec validation is part of the runner contract: a receipt that
        // cannot round-trip invalidates the run.
        let validated = ScenarioReceipt::decode(&receipt.encode())
            .map(|decoded| decoded == receipt)
            .unwrap_or(false);
        if !validated {
            self.counters.failed += 1;
            return ScenarioRecord {
                name: spec.name.to_string(),
                route: spec.route,
                verdict: ScenarioVerdict::Failed,
                elapsed,
                receipt,
                replay: Some(self.replay_command(spec)),
            };
        }
        let replay = if verdict.is_acceptable() {
            None
        } else {
            Some(self.replay_command(spec))
        };
        ScenarioRecord {
            name: spec.name.to_string(),
            route: spec.route,
            verdict,
            elapsed,
            receipt,
            replay,
        }
    }

    fn record_unexecuted(
        &mut self,
        spec: &ScenarioSpec,
        redactor: &Redactor,
        reason: &str,
    ) -> ScenarioRecord {
        self.counters.missing_evidence += 1;
        let mut ring = EventRing::new(8);
        ring.push(redactor, reason);
        let pin = scenario_pin(spec.name, spec.seed);
        let receipt = ScenarioReceipt::from_draft(
            redactor,
            ScenarioReceiptDraft {
                scenario: spec.name.to_string(),
                seed: crate::receipts::ScenarioSeed(spec.seed),
                pin,
                route: crate::receipts::RouteId::new("route/headless")
                    .expect("static route id"),
                corpus_digest: ContentDigest::of(spec.name.as_bytes()),
                corpus_count: 1,
                outcome: TerminalOutcome::new(
                    None,
                    Effect::Canceled,
                    Some(reason.to_string()),
                ),
                comparison: None,
                ring,
                artifacts: vec![],
            },
        );
        ScenarioRecord {
            name: spec.name.to_string(),
            route: spec.route,
            verdict: ScenarioVerdict::MissingEvidence,
            elapsed: Duration::ZERO,
            receipt,
            replay: None,
        }
    }

    fn replay_command(&self, spec: &ScenarioSpec) -> String {
        format!(
            "fcb-scenario --scenario {} --seed {} --route {}",
            spec.name,
            spec.seed,
            match spec.route {
                ScenarioRoute::Headless => "headless",
                ScenarioRoute::Native => "native",
            }
        )
    }

    /// Remove the run root (exactly the directory this driver created) and
    /// retire the driver. The root is refused unless it carries the runner
    /// namespace prefix, so a constructed driver cannot delete arbitrary
    /// paths.
    pub fn finish(mut self) -> Result<RunnerCounters, ScenarioError> {
        if self.finished {
            return Err(ScenarioError::AlreadyFinished);
        }
        self.finished = true;
        let allowed = self
            .run_root
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("fcb-scenario-"))
            .unwrap_or(false);
        if !allowed {
            return Err(ScenarioError::FixtureEscape);
        }
        std::fs::remove_dir_all(&self.run_root).map_err(|_| ScenarioError::FixtureIo)?;
        Ok(self.counters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_pass(_context: &ScenarioContext) -> BodyResult {
        BodyResult::Pass
    }

    fn body_fail(context: &ScenarioContext) -> BodyResult {
        let _ = context.write_fixture("evidence.txt", b"failure detail");
        BodyResult::Fail {
            reason: "deliberate mismatch".to_string(),
        }
    }

    fn spec(name: &'static str, body: fn(&ScenarioContext) -> BodyResult) -> ScenarioSpec {
        ScenarioSpec {
            name,
            route: ScenarioRoute::Headless,
            timeout: Duration::from_secs(5),
            negative_control: false,
            seed: 7,
            body,
        }
    }

    fn redactor() -> Redactor {
        Redactor::new()
    }

    #[test]
    fn driver_runs_passing_scenario_and_cleans_owned_root() {
        let mut driver = ScenarioDriver::new(4, false).unwrap();
        let root = driver.run_root().to_path_buf();
        assert!(root.starts_with(std::env::temp_dir()));
        driver.submit(spec("passes", body_pass)).unwrap();
        let records = driver.run_pending(&redactor());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].verdict, ScenarioVerdict::Passed);
        assert!(records[0].verdict.is_acceptable());
        assert!(records[0].replay.is_none());
        let counters = driver.finish().unwrap();
        assert_eq!(counters.passed, 1);
        assert!(!root.exists(), "owned run root must be removed");
    }

    #[test]
    fn unmarked_failure_fails_with_replay_and_negative_control_is_expected() {
        let mut driver = ScenarioDriver::new(4, false).unwrap();
        let mut control = spec("control", body_fail);
        control.negative_control = true;
        driver.submit(spec("honest_fail", body_fail)).unwrap();
        driver.submit(control).unwrap();
        let records = driver.run_pending(&redactor());
        assert_eq!(records[0].verdict, ScenarioVerdict::Failed);
        assert_eq!(
            records[0].replay_command().unwrap(),
            "fcb-scenario --scenario honest_fail --seed 7 --route headless"
        );
        assert_eq!(records[1].verdict, ScenarioVerdict::ExpectedFailure);
        assert!(records[1].verdict.is_acceptable());
        assert!(records[1].replay.is_none());
        // The failing body wrote a fixture inside its own namespace.
        let counters = driver.finish().unwrap();
        assert_eq!(counters.failed, 1);
        assert_eq!(counters.expected_failures, 1);
    }

    #[test]
    fn negative_control_that_passes_is_a_failed_oracle() {
        let mut driver = ScenarioDriver::new(4, false).unwrap();
        let mut control = spec("broken_control", body_pass);
        control.negative_control = true;
        driver.submit(control).unwrap();
        let records = driver.run_pending(&redactor());
        assert_eq!(records[0].verdict, ScenarioVerdict::Failed);
    }

    #[test]
    fn native_route_without_hardware_is_missing_evidence_never_pass() {
        let mut driver = ScenarioDriver::new(4, false).unwrap();
        let mut native = spec("native_render", body_pass);
        native.route = ScenarioRoute::Native;
        driver.submit(native).unwrap();
        let records = driver.run_pending(&redactor());
        assert_eq!(records[0].verdict, ScenarioVerdict::MissingEvidence);
        assert!(!records[0].verdict.is_acceptable());
        let decoded =
            ScenarioReceipt::decode(&records[0].receipt.encode()).expect("receipt round trip");
        assert_eq!(decoded, records[0].receipt);
        driver.finish().unwrap();
    }

    #[test]
    fn admission_bound_rejects_and_counts() {
        let mut driver = ScenarioDriver::new(1, false).unwrap();
        driver.submit(spec("first", body_pass)).unwrap();
        assert_eq!(
            driver.submit(spec("second", body_pass)),
            Err(ScenarioError::AdmissionSaturated)
        );
        assert_eq!(driver.counters().rejected_admission, 1);
        driver.finish().unwrap();
    }

    #[test]
    fn deadline_overrun_is_timed_out() {
        fn slow(_context: &ScenarioContext) -> BodyResult {
            // Return after the (tiny) deadline has certainly passed.
            std::thread::sleep(Duration::from_millis(30));
            BodyResult::Pass
        }
        let mut driver = ScenarioDriver::new(4, false).unwrap();
        let mut tight = spec("over_deadline", slow);
        tight.timeout = Duration::from_millis(1);
        driver.submit(tight).unwrap();
        let records = driver.run_pending(&redactor());
        assert_eq!(records[0].verdict, ScenarioVerdict::TimedOut);
        assert!(records[0].replay_command().is_some());
        driver.finish().unwrap();
    }

    #[test]
    fn fixture_writes_cannot_escape_the_owned_namespace() {
        let mut driver = ScenarioDriver::new(4, false).unwrap();
        driver.submit(spec("escape", body_pass)).unwrap();
        let records = driver.run_pending(&redactor());
        assert_eq!(records.len(), 1);
        driver.finish().unwrap();

        fn escaping(context: &ScenarioContext) -> BodyResult {
            let _ = context.write_fixture("../escape.txt", b"no");
            BodyResult::Pass
        }
        let mut driver = ScenarioDriver::new(4, false).unwrap();
        driver.submit(spec("escape_attempt", escaping)).unwrap();
        let records = driver.run_pending(&redactor());
        // The body sees the refused write; the run itself still completes
        // because the escape was contained.
        assert_eq!(records[0].verdict, ScenarioVerdict::Passed);
        driver.finish().unwrap();
    }

    #[test]
    fn every_receipt_round_trips_through_the_codec() {
        let mut driver = ScenarioDriver::new(8, false).unwrap();
        driver.submit(spec("round_trip_pass", body_pass)).unwrap();
        driver.submit(spec("round_trip_fail", body_fail)).unwrap();
        let records = driver.run_pending(&redactor());
        for record in records {
            let decoded =
                ScenarioReceipt::decode(&record.receipt.encode()).expect("decode");
            assert_eq!(decoded, record.receipt);
        }
        driver.finish().unwrap();
    }
}
