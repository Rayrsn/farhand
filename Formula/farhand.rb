class Farhand < Formula
  desc "Remote build and test offloader with zero external system binaries"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.11.1"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "6bcb785565eede62ef7a3189f8b4218664dc33cc49d6b6093649d47155cb3526"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "5c2add4e0df1a56145f5270e58d365c112a433d16058eb4b7f49a0a45aee158a"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "02860e3a0a4b2abf16f73bd3c49d8e7c22f8ef049975de4045a968105d27588e"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "f74fb9224c44759918490b03fd9aae9fac0a14179be019c2b3c1367ba53ab213"
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
