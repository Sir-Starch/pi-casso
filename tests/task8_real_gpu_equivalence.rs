#[path = "../src/art.rs"]
#[expect(
    dead_code,
    reason = "the integration harness imports a production module subset"
)]
mod art;
#[path = "../src/gpu.rs"]
mod gpu;
#[path = "../src/gpu_ring.rs"]
#[expect(
    dead_code,
    reason = "the integration harness imports a production module subset"
)]
mod gpu_ring;

use std::path::Path;

use gpu::{GpuSearchEngine, GpuWindowScore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const ARTIFACT: &str =
    ".omo/evidence/task-8-evidence-final-20260822/real-gpu-ring-equivalence-raw.json";
const WINDOWS: usize = 262_144;
const CHUNK_WINDOWS: usize = 4_096;
const CANVAS_WIDTH: usize = 24;
const CANVAS_HEIGHT: usize = 24;
const TARGET_WIDTH: usize = 12;
const TARGET_HEIGHT: usize = 12;
const PI_PREFIX: &[u8] = b"3141592653589793238462643383279502884197169399375105820974944592";
const MOCK_ENV: [&str; 4] = [
    "PI_CASSO_TEST_FAKE_WGPU_PREFLIGHT",
    "PI_CASSO_TEST_FAKE_WGPU_EXECUTION",
    "PI_CASSO_TEST_GPU_COMPLETION_DELAY_MS",
    "PI_CASSO_TEST_BACKEND_FAIL_AFTER_PREFLIGHT",
];

struct Workload {
    digits: Vec<u8>,
    target: art::Bitmap,
}

impl Workload {
    fn new() -> Self {
        let input_len = WINDOWS + CANVAS_WIDTH * CANVAS_HEIGHT - 1;
        let digits = (0..input_len)
            .map(|index| PI_PREFIX[index % PI_PREFIX.len()] - b'0')
            .collect();
        let target = art::load_template("arch", TARGET_WIDTH, TARGET_HEIGHT)
            .expect("the production arch template is valid");
        Self { digits, target }
    }

    fn run(&self, engine: &mut GpuSearchEngine) -> Vec<GpuWindowScore> {
        let window_len = CANVAS_WIDTH * CANVAS_HEIGHT;
        let mut scores = Vec::with_capacity(WINDOWS);
        for start in (0..WINDOWS).step_by(CHUNK_WINDOWS) {
            let windows = CHUNK_WINDOWS.min(WINDOWS - start);
            let end = start + windows + window_len - 1;
            let chunk = engine
                .emergence_scores(
                    &self.digits[start..end],
                    windows,
                    &self.target,
                    CANVAS_WIDTH,
                    CANVAS_HEIGHT,
                )
                .expect("real adapter scores the complete QA chunk");
            assert_eq!(chunk.len(), windows);
            scores.extend(chunk);
        }
        scores
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn vector_digests(scores: &[GpuWindowScore]) -> Value {
    let mut offsets = Sha256::new();
    let mut score_records = Sha256::new();
    let mut combined = Sha256::new();
    for (offset, score) in scores.iter().enumerate() {
        let offset = u64::try_from(offset).expect("QA offset fits u64");
        let offset_bytes = offset.to_le_bytes();
        offsets.update(offset_bytes);
        combined.update(offset_bytes);

        let mut record = Vec::with_capacity(58);
        record.extend_from_slice(&score.score.to_bits().to_le_bytes());
        record.push(score.digit);
        record.extend_from_slice(
            &u64::try_from(score.x)
                .expect("QA x coordinate fits u64")
                .to_le_bytes(),
        );
        record.extend_from_slice(
            &u64::try_from(score.y)
                .expect("QA y coordinate fits u64")
                .to_le_bytes(),
        );
        record.extend_from_slice(&score.coverage.to_bits().to_le_bytes());
        record.extend_from_slice(&score.leakage.to_bits().to_le_bytes());
        match score.statistics.as_ref() {
            Some(statistics) => {
                record.push(1);
                record.extend_from_slice(&statistics.covered.to_le_bytes());
                record.extend_from_slice(&statistics.total.to_le_bytes());
                record.extend_from_slice(&statistics.leaked.to_le_bytes());
                record.extend_from_slice(&statistics.background_total.to_le_bytes());
            }
            None => record.push(0),
        }
        score_records.update(&record);
        combined.update(&record);
    }
    json!({
        "length": scores.len(),
        "offsets_sha256": format!("{:x}", offsets.finalize()),
        "score_records_sha256": format!("{:x}", score_records.finalize()),
        "offset_score_pairs_sha256": format!("{:x}", combined.finalize()),
    })
}

fn write_artifact(value: &Value) {
    let path = Path::new(ARTIFACT);
    assert!(
        !path.exists(),
        "refusing to overwrite immutable QA artifact"
    );
    std::fs::create_dir_all(path.parent().expect("artifact has a parent"))
        .expect("create QA evidence directory");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize QA artifact"),
    )
    .expect("write QA artifact");
}

#[test]
#[ignore = "capability-gated real-adapter QA; writes an immutable evidence artifact"]
fn real_gpu_ring_depths_match_full_vector() {
    // Given: no fake/mock controls and the plan's 262144/4096 workload.
    for variable in MOCK_ENV {
        assert!(
            std::env::var_os(variable).is_none(),
            "real-adapter QA refuses mock/failpoint environment {variable}"
        );
    }
    let workload = Workload::new();
    let Ok(mut serial) = GpuSearchEngine::new_with_depth(None, 1) else {
        write_artifact(&json!({
            "schema_version": 1,
            "kind": "task8_real_gpu_full_vector_equivalence",
            "status": "skip",
            "capability_state": "unavailable",
            "reason": "adapter_device_or_pipeline_preflight_unavailable",
            "test_only_mock": false,
            "workload": {
                "work_windows": WINDOWS,
                "chunk_windows": CHUNK_WINDOWS,
                "chunks": WINDOWS / CHUNK_WINDOWS,
            },
        }));
        eprintln!("SKIP: wgpu adapter/device/pipeline preflight unavailable");
        return;
    };
    let adapter = serial.device_name().to_string();
    let mut depth_two =
        GpuSearchEngine::new_with_depth(None, 2).expect("depth two uses preflight-ok adapter");
    let mut depth_four =
        GpuSearchEngine::new_with_depth(None, 4).expect("depth four uses preflight-ok adapter");
    assert_eq!(depth_two.device_name(), adapter);
    assert_eq!(depth_four.device_name(), adapter);

    // When: the production depth-one serial path and bounded rings score every chunk.
    let expected = workload.run(&mut serial);
    let at_two = workload.run(&mut depth_two);
    let at_four = workload.run(&mut depth_four);

    // Then: all ordered score records and their implied absolute offsets are exact.
    assert_eq!(expected.len(), WINDOWS);
    assert_eq!(at_two, expected);
    assert_eq!(at_four, expected);
    let serial_digests = vector_digests(&expected);
    let depth_two_digests = vector_digests(&at_two);
    let depth_four_digests = vector_digests(&at_four);
    assert_eq!(depth_two_digests, serial_digests);
    assert_eq!(depth_four_digests, serial_digests);

    write_artifact(&json!({
        "schema_version": 1,
        "kind": "task8_real_gpu_full_vector_equivalence",
        "status": "pass",
        "capability_state": "preflight_ok",
        "test_only_mock": false,
        "adapter": adapter,
        "production_api": "gpu::GpuSearchEngine::emergence_scores",
        "serial_reference": "production wgpu ring depth 1",
        "workload": {
            "template": "arch",
            "target_width": TARGET_WIDTH,
            "target_height": TARGET_HEIGHT,
            "target_bitmap_sha256": workload.target.sha256(),
            "canvas_width": CANVAS_WIDTH,
            "canvas_height": CANVAS_HEIGHT,
            "work_windows": WINDOWS,
            "chunk_windows": CHUNK_WINDOWS,
            "chunks": WINDOWS / CHUNK_WINDOWS,
            "input_digits_length": workload.digits.len(),
            "input_digits_sha256": sha256(&workload.digits),
        },
        "digest_encoding": "for each absolute offset in order: offset u64 LE; score f64 bits LE; digit u8; x u64 LE; y u64 LE; coverage f64 bits LE; leakage f64 bits LE; statistics-present u8; when present covered,total,leaked,background_total as u32 LE",
        "depths": [
            {"depth": 1, "role": "serial_reference", "vectors": serial_digests},
            {"depth": 2, "role": "bounded_ring", "vectors": depth_two_digests},
            {"depth": 4, "role": "bounded_ring", "vectors": depth_four_digests},
        ],
        "full_score_offset_equality": true,
    }));
}
