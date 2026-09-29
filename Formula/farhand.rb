class Farhand < Formula
  desc "Remote build and test offloader with zero external system binaries"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.10.0"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "e1c547dcefa6bccde13956b5c7dd06351776e6f50c2e6049417d5decf97c52bc"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "43e5a3504e4e134439746b10705c1f02f1351b760987381e34ffe729fbdc4ff8"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "7a0e525794deb227356a6e23dc631aa6187e599a1ffd55429cd6dce9eb578008"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "65debf68b85fad2ec30450c4295e8aeee418a10fef87d10f443b361f65b81d2e"
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
