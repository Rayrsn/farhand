class Farhand < Formula
  desc "Remote build and test offloader with zero external system binaries"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.10.1"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "6b3bad316fddd5c07d8dce6ddc4b419d2261029a0632c0b4073e8f06ee135cec"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "9edf62808460909324592b3a328ad85a5cf11277f66f0f7d9b7497d650c8e72a"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "233f9a5b4c9e0c77a00f75124790f90d713b8d37f923789bcaaf6f24a2324cee"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "a77d83d28c8d0e604726fa827193449e1eed534cdf3aea59959dae9ae4ca3238"
    end
  end

  def install
    bin.install "fh"
    bin.install "fhd"
  end

  # Deliberately no `service do` block. A formula service definition is
  # started by a plain `brew services start farhand`, with no prompt and no
  # chance to supply a secret — so any token baked into it is a published
  # token guarding a daemon that executes arbitrary commands. The one that was
  # here bound 0.0.0.0:9876 with FARHAND_TOKEN "replace-with-your-token".
  #
  # For a long-running agent, install one of the units in dist/services/ and
  # set a real token (openssl rand -hex 32) first. See
  # docs/mac-build-server-setup.md.

  test do
    assert_match "Farhand client", shell_output("#{bin}/fh --help")
    assert_match "Farhand daemon", shell_output("#{bin}/fhd --help")
  end
end
