use std::fs;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "fixture barrier timed out: {path:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn race(root: &Path, mode: &str) {
    let mut children: Vec<_> = (0..2)
        .map(|slot| {
            OwnedChild(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "writer_process"])
                    .env("ESI_CAS_TEST_ROOT", root)
                    .env("ESI_CAS_TEST_MODE", mode)
                    .env("ESI_CAS_TEST_SLOT", slot.to_string())
                    .spawn()
                    .unwrap(),
            )
        })
        .collect();
    for slot in 0..2 {
        wait_for(&root.join(format!("ready-{slot}")));
    }
    fs::write(root.join("go"), "").unwrap();
    let mut outcomes: Vec<_> = children
        .iter_mut()
        .map(|child| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status.code().unwrap();
                }
                assert!(Instant::now() < deadline, "writer process timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .collect();
    outcomes.sort();
    assert_eq!(
        outcomes,
        [0, 2],
        "exactly one writer must commit and one must conflict"
    );
}
