docker build \
    -f Dockerfile -t pong-server:latest ../../../../

#   --build-arg CARGO_BUILD_FEATURES="matchmaker/run_migrations,matchmaker/telegram,game/$NETWORK" \
