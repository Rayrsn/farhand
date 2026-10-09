class Farhand < Formula
  desc "Remote build and test offloader with zero external system binaries"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.11.0"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "548436e3c76a829191a6ee38147754180d72fc42fcb225c2c19913d25943dacf"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "2024a33a3a9fb87d559ed86ff7673382a15064d0ea38b8f94110b65a853f98d0"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "386fcdd4aa06b42cdfd9e15e1f7070eb21464863c1e02db7b776b32a3cb90bdf"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d22ce2437d400d69ab2f3d5bd10023b3780fd6c171047be9f519d71e0c347f72"
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
