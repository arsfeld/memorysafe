{ pkgs, lib, ... }:

{
  # Pinned Rust toolchain via rust-toolchain.toml (Rust 1.97.1 with rustfmt and clippy)
  languages.rust = {
    enable = true;
    toolchainFile = ./rust-toolchain.toml;
  };

  # Additional developer tooling
  packages = [
    pkgs.git
    pkgs.python3 # required by scripts/mutate
  ];

  # Provide C++ runtime library (libstdc++.so.6) on Nix-based systems so bundled SQLite links cleanly
  env.LD_LIBRARY_PATH = "${lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib ]}";

  enterShell = ''
    echo "MemorySafe dev environment active (Rust $(rustc --version | cut -d' ' -f2))"
  '';
}
