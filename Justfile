

install:
    cargo build --release
    sudo cp target/release/demarc /usr/local/bin

test:
    cargo test

transcode:
    ffmpeg -i demo.mp4 \
        -c:v libx264 -preset slow -crf 23 -profile:v high -pix_fmt yuv420p \
        -x264-params "keyint=100:min-keyint=50" \
        -color_primaries bt709 -color_trc bt709 -colorspace bt709 \
        -c:a aac -b:a 384k -ar 48000 \
        -movflags +faststart \
        demo_youtube.mp4
