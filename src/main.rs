use std::net::UdpSocket;
use std::process::Command;
use std::time::{Duration, Instant};

// smoothing factor for exponential low pass filter (0.0 = no smoothing, 1.0 = frozen)
// higher values can be smoother but more latency. keep it between 0.7-0.85
const SMOOTHING_FACTOR: f64 = 0.75;

// yaw rotation needed for full left/right pan
// lower = more sensitive, default: 30.0 (±30° for full pan)
const YAW_SENSITIVITY: f64 = 30.0;

// degrees for pitch for volume adjustment range
// looking up by this amount = MAX_VOLUME, down = MIN_VOLUME
const PITCH_SENSITIVITY: f64 = 20.0;

// dead zone in center, no panning within this range
const DEAD_ZONE: f64 = 5.0;

// min time between updates in ms (33ms ~= 30fps, 50ms = 20fps)
const UPDATE_RATE_MS: u64 = 40;

// vol range for pitch control
const MIN_VOLUME: f64 = 0.3;
const MAX_VOLUME: f64 = 1.0;

// min channel volume (makes sure there's no complete silence on one side)
const MIN_CHANNEL: f64 = 0.05;

struct SmoothedState {
    yaw: f64,
    pitch: f64,
    roll: f64,
}

impl SmoothedState {
    fn new() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
        }
    }

    // apply exponential smoothing: smoothed = α * previous + (1 - α) * current
    fn update(&mut self, raw_yaw: f64, raw_pitch: f64, raw_roll: f64) {
        self.yaw = SMOOTHING_FACTOR * self.yaw + (1.0 - SMOOTHING_FACTOR) * raw_yaw;
        self.pitch = SMOOTHING_FACTOR * self.pitch + (1.0 - SMOOTHING_FACTOR) * raw_pitch;
        self.roll = SMOOTHING_FACTOR * self.roll + (1.0 - SMOOTHING_FACTOR) * raw_roll;
    }
}

// audio control
struct AudioState {
    left: f64,
    right: f64,
    volume: f64,
    effective_yaw: f64,
}

impl AudioState {
    fn from_head_tracking(yaw: f64, pitch: f64) -> Self {
        // apply dead zone to yaw
        let effective_yaw = if yaw.abs() < DEAD_ZONE {
            0.0
        } else {
            // rm dead zone from the value
            let sign = yaw.signum();
            sign * (yaw.abs() - DEAD_ZONE)
        };

        // normalize yaw to pan: -YAW_SENSITIVITY..+YAW_SENSITIVITY -> 0..1
        let max_yaw = YAW_SENSITIVITY - DEAD_ZONE;
        let normalized = (effective_yaw.clamp(-max_yaw, max_yaw) / max_yaw + 1.0) / 2.0;

        // calculate stereo balance
        let left = (1.0 - normalized).max(MIN_CHANNEL);
        let right = normalized.max(MIN_CHANNEL);

        //  calculate volume (pitch), looking up = louder vice versa
        let pitch_normalized = (pitch.clamp(-PITCH_SENSITIVITY, PITCH_SENSITIVITY)
            / PITCH_SENSITIVITY + 1.0) / 2.0;
        let volume = MIN_VOLUME + pitch_normalized * (MAX_VOLUME - MIN_VOLUME);

        Self {
            left: left * volume,
            right: right * volume,
            volume,
            effective_yaw,
        }
    }
}

// display

// clear screen and move cursor to top-left
fn clear_screen() {
    print!("\x1B[2J\x1B[1;1H");
}

// Helper: Calculate string width ignoring ANSI color codes
// Fixes border alignment by counting emojis as 2 width
fn get_visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut inside_ansi = false;
    for c in s.chars() {
        if c == '\x1B' {
            inside_ansi = true;
            continue;
        }
        if inside_ansi {
            if c == 'm' {
                inside_ansi = false;
            }
            continue;
        }
        // Account for double-width emojis used in headers
        match c {
            '🎧' | '📊' | '🔊' | '📈' => width += 2,
            _ => width += 1,
        }
    }
    width
}

// create an ASCII pan indicator bar
fn render_pan_bar(yaw: f64, width: usize) -> String {
    let half = width / 2;
    let normalized = (yaw.clamp(-YAW_SENSITIVITY, YAW_SENSITIVITY) / YAW_SENSITIVITY + 1.0) / 2.0;
    let pos = (normalized * (width - 1) as f64).round() as usize;

    let mut bar = String::with_capacity(width + 10);
    bar.push('[');

    for i in 0..width {
        if i == half {
            if pos == i {
                bar.push_str("\x1B[1;33m┃\x1B[0m"); // yellow center marker when at center
            } else {
                bar.push('│');
            }
        } else if i == pos {
            if i < half {
                bar.push_str("\x1B[1;33m◀\x1B[0m"); // blue left indicator
            } else {
                bar.push_str("\x1B[1;35m▶\x1B[0m"); // red right indicator
            }
        } else if i < half {
            if i >= pos && pos < half {
                bar.push_str("\x1B[33m━\x1B[0m");
            } else {
                bar.push('─');
            }
        } else {
            if i <= pos && pos > half {
                bar.push_str("\x1B[35m━\x1B[0m");
            } else {
                bar.push('─');
            }
        }
    }

    bar.push(']');
    bar
}

// vol bar
fn render_volume_bar(volume: f64, width: usize) -> String {
    let filled = ((volume / MAX_VOLUME) * width as f64).round() as usize;
    let mut bar = String::with_capacity(width + 10);
    bar.push('[');

    for i in 0..width {
        if i < filled {
            let intensity = i as f64 / width as f64;
            if intensity < 0.5 {
                bar.push_str("\x1B[32m█\x1B[0m"); // green
            } else if intensity < 0.8 {
                bar.push_str("\x1B[33m█\x1B[0m"); // yellow
            } else {
                bar.push_str("\x1B[31m█\x1B[0m"); // red
            }
        } else {
            bar.push('░');
        }
    }

    bar.push(']');
    bar
}

fn render_dashboard(
    smoothed: &SmoothedState,
    raw_yaw: f64,
    raw_pitch: f64,
    raw_roll: f64,
    audio: &AudioState,
    fps: f64,
    streams: usize,
    packets: u64,
    latency_ms: f64,
) {
    clear_screen();

    // Helper closure to draw a bordered row with dynamic padding
    // fixes the "sticking with text" issue by ensuring strict 66-char inner width
    let draw_row = |content: &str| {
        let inner_target = 66;
        let visible = get_visible_width(content);
        let padding = if inner_target > visible { inner_target - visible } else { 0 };
        println!("\x1B[1;96m║\x1B[0m{}{}\x1B[1;96m║\x1B[0m", content, " ".repeat(padding));
    };

    // Helper to pad a specific field to a visual width, preserving ANSI codes
    let pad_field = |text: String, width: usize| -> String {
        let vis = get_visible_width(&text);
        let p = if width > vis { width - vis } else { 0 };
        format!("{}{}", text, " ".repeat(p))
    };

    println!("\x1B[1;96m╔══════════════════════════════════════════════════════════════════╗\x1B[0m");
    // Header - manual center calc for simplicity
    let title = "\x1B[1;37m🎧 HEAD TRACKING AUDIO PANNER\x1B[0m";
    let t_vis = get_visible_width(title);
    let t_pad = (66 - t_vis) / 2;
    println!("\x1B[1;96m║\x1B[0m{}{}{}\x1B[1;96m║\x1B[0m", " ".repeat(t_pad), title, " ".repeat(66 - t_vis - t_pad));
    println!("\x1B[1;96m╠══════════════════════════════════════════════════════════════════╣\x1B[0m");

    // raw vs smoothed
    draw_row(&format!("  {}", "\x1B[1;33m📊 HEAD ORIENTATION\x1B[0m"));
    draw_row("");
    draw_row(&format!("    {}",
                      format!("\x1B[90mRAW:\x1B[0m     Yaw={:>7.1}°  Pitch={:>7.1}°  Roll={:>7.1}°",
                              raw_yaw, raw_pitch, raw_roll)));
    draw_row(&format!("    {}",
                      format!("\x1B[1;37mSMOOTH:\x1B[0m  Yaw={:>7.1}°  Pitch={:>7.1}°  Roll={:>7.1}°",
                              smoothed.yaw, smoothed.pitch, smoothed.roll)));

    // dead zone
    let dead_zone_status = if smoothed.yaw.abs() < DEAD_ZONE {
        "\x1B[1;32m● DEAD ZONE (centered)\x1B[0m"
    } else {
        "\x1B[90m○ active tracking\x1B[0m"
    };
    draw_row(&format!("    Status: {}", dead_zone_status));

    draw_row("");
    println!("\x1B[1;96m╠══════════════════════════════════════════════════════════════════╣\x1B[0m");

    // audio section
    draw_row(&format!("  {}", "\x1B[1;35m🔊 AUDIO OUTPUT\x1B[0m"));
    draw_row("");

    // pan bar
    let pan_bar = render_pan_bar(audio.effective_yaw, 40);
    draw_row(&format!("    \x1B[1;37mPAN:\x1B[0m  L {} R", pan_bar));

    // channel levels
    draw_row(&format!("          Left={:.2}  Right={:.2}  (effective yaw: {:>+.1}°)",
                      audio.left, audio.right, audio.effective_yaw));

    draw_row("");

    // vol bar
    let vol_bar = render_volume_bar(audio.volume, 40);
    draw_row(&format!("    \x1B[1;37mVOL:\x1B[0m  {} {:>3.0}%", vol_bar, audio.volume * 100.0));

    // pitch indicator
    let pitch_indicator = if smoothed.pitch > 5.0 {
        "↑ looking UP (louder)"
    } else if smoothed.pitch < -5.0 {
        "↓ looking DOWN (quieter)"
    } else {
        "─ level"
    };
    draw_row(&format!("          {}", pitch_indicator));

    draw_row("");
    println!("\x1B[1;96m╠══════════════════════════════════════════════════════════════════╣\x1B[0m");

    // stats section
    draw_row(&format!("  {}", "\x1B[1;32m📈 STATS\x1B[0m"));
    draw_row("");

    // Stats alignment logic (2 columns)
    let col_width = 25;

    // Row 1
    let fps_str = pad_field(format!("FPS: \x1B[1;37m{:>5.1}\x1B[0m", fps), col_width);
    let lat_str = format!("Latency: \x1B[1;37m{:.2}ms\x1B[0m", latency_ms);
    draw_row(&format!("    {}  │  {}", fps_str, lat_str));

    // Row 2
    let strm_str = pad_field(format!("Streams: \x1B[1;37m{:>2}\x1B[0m", streams), col_width);
    let pkts_str = format!("Packets: \x1B[1;37m{:>8}\x1B[0m", packets);
    draw_row(&format!("    {}  │  {}", strm_str, pkts_str));

    // Row 3
    let smooth_str = pad_field(format!("Smoothing: {:.0}%", SMOOTHING_FACTOR * 100.0), col_width);
    let dead_str = format!("Dead zone: ±{}°", DEAD_ZONE);
    draw_row(&format!("    {}  │  {}", smooth_str, dead_str));

    draw_row("");
    println!("\x1B[1;96m╠══════════════════════════════════════════════════════════════════╣\x1B[0m");
    draw_row(&format!("  {}", "\x1B[90mPress Ctrl+C to exit\x1B[0m"));
    println!("\x1B[1;96m╚══════════════════════════════════════════════════════════════════╝\x1B[0m");
}
fn main() {
    // initial setup display
    clear_screen();
    println!("\x1B[1;96m╔══════════════════════════════════════════════════════════════════╗\x1B[0m");
    println!("\x1B[1;96m║\x1B[0m{:^66}\x1B[1;96m║\x1B[0m", "\x1B[1;37m🎧 HEAD TRACKING AUDIO PANNER\x1B[0m");
    println!("\x1B[1;96m╠══════════════════════════════════════════════════════════════════╣\x1B[0m");
    println!("\x1B[1;96m║\x1B[0m{:66}\x1B[1;96m║\x1B[0m", "");
    println!("\x1B[1;96m║\x1B[0m  {:<64}\x1B[1;96m║\x1B[0m", "🔌 Binding to UDP port 4242...");

    let socket = match UdpSocket::bind("127.0.0.1:4242") {
        Ok(s) => {
            println!("\x1B[1;96m║\x1B[0m  {:<64}\x1B[1;96m║\x1B[0m", "\x1B[1;32m✓ Socket bound successfully!\x1B[0m");
            s
        }
        Err(e) => {
            eprintln!("\x1B[1;96m║\x1B[0m  \x1B[1;31m✗ FAILED to bind socket: {}\x1B[0m", e);
            std::process::exit(1);
        }
    };

    socket
        .set_read_timeout(Some(Duration::from_millis(UPDATE_RATE_MS / 2)))
        .expect("Failed to set timeout");

    println!("\x1B[1;96m║\x1B[0m{:66}\x1B[1;96m║\x1B[0m", "");
    println!("\x1B[1;96m║\x1B[0m  {:<64}\x1B[1;96m║\x1B[0m", "\x1B[1;33m⏳ Waiting for OpenTrack data...\x1B[0m");
    println!("\x1B[1;96m║\x1B[0m     {:<61}\x1B[1;96m║\x1B[0m", "Make sure OpenTrack is sending UDP to 127.0.0.1:4242");
    println!("\x1B[1;96m║\x1B[0m{:66}\x1B[1;96m║\x1B[0m", "");
    println!("\x1B[1;96m╚══════════════════════════════════════════════════════════════════╝\x1B[0m");

    let mut buf = [0u8; 48];
    let mut last_update = Instant::now();
    let mut last_fps_calc = Instant::now();
    let mut packet_count: u64 = 0;
    let mut frame_count: u32 = 0;
    let mut current_fps: f64 = 0.0;
    let mut stream_count: usize = 0;
    let mut smoothed = SmoothedState::new();
    let mut first_packet = true;

    // raw values for display comp
    let (mut raw_yaw, mut raw_pitch, mut raw_roll) = (0.0_f64, 0.0_f64, 0.0_f64);

    // latency tracking
    let mut latency_samples: Vec<f64> = Vec::with_capacity(30);
    let mut avg_latency_ms: f64 = 0.0;

    loop {
        match socket.recv_from(&mut buf) {
            Ok((amt, _addr)) => {
                let recv_time = Instant::now();
                packet_count += 1;

                if amt != 48 {
                    continue;
                }

                // parse
                let data: [f64; 6] = unsafe { std::mem::transmute(buf) };
                raw_yaw = data[3];
                raw_pitch = data[4];
                raw_roll = data[5];

                // smoothing
                smoothed.update(raw_yaw, raw_pitch, raw_roll);

                // rate limit display updates
                if last_update.elapsed() < Duration::from_millis(UPDATE_RATE_MS) {
                    continue;
                }

                if first_packet {
                    first_packet = false;
                }

                // calculate FPS
                frame_count += 1;
                if last_fps_calc.elapsed() >= Duration::from_secs(1) {
                    current_fps = frame_count as f64 / last_fps_calc.elapsed().as_secs_f64();
                    frame_count = 0;
                    last_fps_calc = Instant::now();
                }

                // calculate audio parameters
                let audio = AudioState::from_head_tracking(smoothed.yaw, smoothed.pitch);

                // measure end-to-end latency
                let pre_audio = Instant::now();
                stream_count = set_all_streams_pan(audio.left, audio.right);
                let audio_latency = pre_audio.elapsed().as_secs_f64() * 1000.0;

                // track latency samples
                latency_samples.push(audio_latency);
                if latency_samples.len() > 30 {
                    latency_samples.remove(0);
                }
                avg_latency_ms = latency_samples.iter().sum::<f64>() / latency_samples.len() as f64;

                // render dashboard
                render_dashboard(
                    &smoothed,
                    raw_yaw,
                    raw_pitch,
                    raw_roll,
                    &audio,
                    current_fps,
                    stream_count,
                    packet_count,
                    avg_latency_ms,
                );

                last_update = Instant::now();
            }
            Err(e) => {
                if e.kind() != std::io::ErrorKind::WouldBlock
                    && e.kind() != std::io::ErrorKind::TimedOut {
                    eprintln!("❌ Socket error: {}", e);
                }
            }
        }
    }
}

// pipewire control
fn set_all_streams_pan(left: f64, right: f64) -> usize {
    let output = match Command::new("pw-cli")
        .args(["list-objects", "Node"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return 0,
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let mut updated_count = 0;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.trim().starts_with("id") && line.contains("PipeWire:Interface:Node") {
            if let Some(id_str) = line.split_whitespace().nth(1) {
                let id = id_str.trim_end_matches(',');

                let mut j = i + 1;
                let mut is_audio_output = false;

                while j < lines.len() && j < i + 20 {
                    let check_line = lines[j];

                    if check_line.trim().starts_with("id") {
                        break;
                    }

                    if check_line.contains("media.class")
                        && check_line.contains("Stream/Output/Audio")
                    {
                        is_audio_output = true;
                    }

                    j += 1;
                }

                if is_audio_output {
                    let result = Command::new("pw-cli")
                        .args([
                            "set-param",
                            id,
                            "Props",
                            &format!("{{ \"channelVolumes\": [{:.3}, {:.3}] }}", left, right),
                        ])
                        .output();

                    if let Ok(out) = result {
                        if out.status.success() {
                            updated_count += 1;
                        }
                    }
                }
            }
        }

        i += 1;
    }

    updated_count
}