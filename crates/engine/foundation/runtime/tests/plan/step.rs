use std::sync::Arc;

use coblet_types::FileHash;

use super::{
    ArtifactKind, CustomStep, DownloadStep, Expectation, ProcessArg, ProcessStep, Program, Sources,
    Step,
};
use crate::{BoxFuture, HttpUrl, PlanPath, RelPath, StepContext, StepError, StepOutcome};

fn path(root: fn(RelPath) -> PlanPath, rel: &str) -> PlanPath {
    root(RelPath::new(rel).unwrap())
}

#[test]
fn serializes_steps_with_a_type_tag() {
    let step = Step::Download(DownloadStep {
        sources: Sources::single(HttpUrl::new("https://libraries.minecraft.net/a.jar").unwrap()),
        expect: Expectation::hash(
            FileHash::new("sha1", "da39a3ee5e6b4b0d3255bfef95601890afd80709").unwrap(),
            Some(0),
        ),
        dest: path(PlanPath::data, "libraries/a.jar"),
        kind: ArtifactKind::Library,
    });
    let json = serde_json::to_value(&step).unwrap();
    assert_eq!(json["type"], "download");
    assert_eq!(json["expect"]["check"], "hashes");
    assert_eq!(
        json["expect"]["primary"],
        "sha1:da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    assert_eq!(serde_json::from_value::<Step>(json).unwrap(), step);
}

#[test]
fn describes_processes_with_plan_paths() {
    let step = Step::RunProcess(ProcessStep {
        program: Program::Java,
        args: vec![
            ProcessArg::Literal("-cp".to_owned()),
            ProcessArg::ClassPath(vec![path(PlanPath::data, "libraries/p.jar")]),
            ProcessArg::Path(path(PlanPath::staging, "client.jar")),
        ],
        cwd: None,
        outputs: Vec::new(),
    });
    let text = serde_json::to_string(&step).unwrap();
    assert_eq!(serde_json::from_str::<Step>(&text).unwrap(), step);
}

#[test]
fn refuses_to_serialize_custom_steps() {
    #[derive(Debug)]
    struct Patch;
    impl CustomStep for Patch {
        fn kind(&self) -> &'static str {
            "acme:patch"
        }
        fn describe(&self) -> String {
            "patch options.txt".to_owned()
        }
        fn run<'a>(
            &'a self,
            _cx: &'a StepContext,
        ) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
            Box::pin(async { Ok(StepOutcome::Succeeded) })
        }
    }
    let shared: Arc<dyn CustomStep> = Arc::new(Patch);
    let step = Step::Custom(Arc::clone(&shared));
    assert!(serde_json::to_string(&step).is_err());
    assert_eq!(step, Step::Custom(shared));
    assert_ne!(step, Step::Custom(Arc::new(Patch)));
}

#[test]
fn reports_expectations() {
    let unverified = Expectation::Unverified { size: Some(10) };
    assert!(!unverified.is_verified());
    assert_eq!(unverified.size(), Some(10));
    let urls = Sources {
        primary: HttpUrl::new("https://a.example/x").unwrap(),
        fallbacks: vec![HttpUrl::new("https://b.example/x").unwrap()],
    };
    assert_eq!(urls.iter().count(), 2);
}
