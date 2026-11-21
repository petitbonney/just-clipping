//! Proves the native dependencies of the analysis stages build and run.
//!
//! Usage: native-deps-spike [path/to/face_detection_yunet.onnx]
//!
//! With a model path, the enabled ONNX backend(s) load YuNet and run one
//! inference on a blank 640x640 frame. Exits non-zero on any failure.

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("whisper.cpp: {}", whisper_rs::print_system_info());

    let model = std::env::args().nth(1);

    #[cfg(feature = "ort")]
    {
        let _ = ort::init().with_name("jc-spike").commit();
        println!("ort: ONNX Runtime initialised");
        if let Some(path) = &model {
            let session = ort::session::Session::builder()?.commit_from_file(path)?;
            println!("ort: loaded {path} ({} inputs)", session.inputs().len());
        }
    }

    #[cfg(feature = "tract")]
    if let Some(path) = &model {
        use tract_onnx::prelude::*;
        let plan = tract_onnx::onnx()
            .model_for_path(path)?
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
    }

    let _ = model;
    Ok(())
}
