//! Proves the analysis stages' dependencies build and run.
//!
//! Usage: native-deps-spike path/to/face_detection_yunet.onnx
//!
//! Prints whisper.cpp's system info, then loads YuNet with tract and runs
//! one inference on a blank 640x640 frame. Exits non-zero on any failure.

use std::error::Error;
use tract_onnx::prelude::*;

fn main() -> Result<(), Box<dyn Error>> {
    println!("whisper.cpp: {}", whisper_rs::print_system_info());

    let model = std::env::args()
        .nth(1)
        .ok_or("usage: native-deps-spike <yunet.onnx>")?;
    let plan = tract_onnx::onnx()
        .model_for_path(&model)?
        .with_input_fact(0, f32::fact([1, 3, 640, 640]).into())?
        .into_optimized()?
        .into_runnable()?;
    let frame = Tensor::zero::<f32>(&[1, 3, 640, 640])?;
    let start = std::time::Instant::now();
    let outputs = plan.run(tvec!(frame.into()))?;
    println!(
        "tract: YuNet ran in {:?}, {} outputs",
        start.elapsed(),
        outputs.len()
    );
    Ok(())
}
