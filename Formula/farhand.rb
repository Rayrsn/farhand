class Farhand < Formula
  desc "Remote build and test offloader with zero external system binaries"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.10.2"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "d7f32c7f2f1243dce85a77639ccc6850ed89c637d0c0fcd00a60632910866138"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "d6abd10c758291d0549526100d9da3e0f5ed8a3e237e27b71ce6d0f151838b74"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "d57230d7f6d589c4ba641d5f5a75f9b42a410a4fcd4caa9941a13dd41cbf834e"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "2a5f4a9b4866ac0018ceccccdb8870e33909214fd902315e4e3cd0a8b7277bcd"
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
