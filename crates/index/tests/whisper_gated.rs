use majestical_index::model::{self, WHISPER};
use majestical_index::transcribe::Transcriber;
use majestical_index::video;

#[test]
#[ignore = "needs fetched whisper model and ffmpeg + say on PATH"]
fn transcribes_spoken_fixture_with_sane_timestamps() {
    let dir = model::model_dir_for(&WHISPER).expect("dir");
    assert!(
        dir.join(majestical_index::transcribe::MODEL_FILE).is_file(),
        "run `maj model fetch --only whisper-large-v3-turbo-q5-v1` first"
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let aiff = tmp.path().join("fixture.aiff");
    let status = std::process::Command::new("say")
        .args(["-o"])
        .arg(&aiff)
        .arg("The quick brown fox jumps over the lazy dog")
        .status()
        .expect("say");
    assert!(status.success());
    let pcm = video::extract_audio_pcm(&aiff, 10_000).expect("pcm");
    let transcriber = Transcriber::load(&dir).expect("load");
    let transcript = transcriber.transcribe(&pcm).expect("transcribe");
    let lower = transcript.text.to_lowercase();
    assert!(lower.contains("quick brown fox"), "got: {lower}");
    assert!(!transcript.segments.is_empty());
    assert!(transcript.segments[0].end_ms > transcript.segments[0].start_ms);
}
