Spatial audio for Linux.

# Requirements 
- Pipewire
- [Opentrack](https://github.com/opentrack/opentrack/releases/tag/opentrack-2026.1.0) (`.exe` with wine, Outputing `UDP over network` with `NeuralNetwork Tracker`)

# Installation
1. Clone this repository
2. Move config files to `~/.config/pipewire/pipewire.conf.d/`
```bash
    mkdir -p ~/.config/pipewire/pipewire.conf.d/
    mv conf/99-spatializer.conf ~/.config/pipewire/pipewire.conf.d/99-spatializer.conf
``` 
3. Move SOFA and reverb .wav files. 
```bash
    mkdir -p /usr/share/pipewire/sofa/
    mv assets/subject_021.sofa /usr/share/pipewire/sofa/

    mkdir -p /usr/share/pipewire/convolver/
    mv assets/reverb.wav /usr/share/pipewire/convolver/
```
4. Restart pipewire
```bash
    systemctl --user restart pipewire pipewire-pulse
```
5. Make sure opentrack is running and Inputting `NeuralNetwork Tracker` and Outputing `UDP over network` to `127.0.0.1:4242` 
![screenshot](/assets/opentrack.png)

6. Run with `cargo run` or install 
```bash
cargo build --release --target-dir ./target
sudo cp target/release/spatial-track /usr/local/bin/ 
```
![screenshot](/assets/demo.png)
