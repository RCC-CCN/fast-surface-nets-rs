# Use the official Rust devcontainer image as a base
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

# ********************************************************
# * Anything else you want to do like clean up goes here *
# ********************************************************

# Install the necessary system libraries
RUN apt-get update && apt-get install -y \   
    build-essential \
    cmake \ 
    ninja-build \ 
    libxinerama-dev \ 
    pkg-config \    
    libasound2-dev \ 
    libudev-dev \   
    mesa-utils \ 
    vulkan-tools \ 
    libwayland-dev \ 
    libxkbcommon-dev \   
    libvulkan1 \    
    libvulkan-dev \ 
    libegl1-mesa-dev \   
    libgles2-mesa-dev \  
    libx11-dev \    
    libxcursor-dev \
    libxrandr-dev \
    libxi-dev \
    libxrandr-dev \
    libxcb1-dev \
    libxcb-icccm4-dev \
    libxcb-image0-dev \
    libxcb-keysyms1-dev \
    libxcb-randr0-dev \
    libxcb-shape0-dev \
    libxcb-xfixes0-dev \
    libxcb-xkb-dev \
    libegl1-mesa \
    libgl1-mesa-glx \
    libgl1-mesa-dri \
    libglu1-mesa-dev \
    libglu1-mesa \
    libgles2-mesa \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

    # Ensure Rust components are available
RUN rustup component add rust-src clippy rustfmt

# (Optional) Add any global cargo installs here, e.g. wasm-bindgen
# RUN cargo install --locked wasm-bindgen-cli
