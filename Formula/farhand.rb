class Farhand < Formula
  desc "Zero-dependency remote build and test offloader for weak machines"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "c513385683c6bd73bfb1eaa4ff99611d87810dda811e321c204df032d38a94c4"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "f392ebc94ee8569b43582797d55b2c34f672c00d9955e3721e3e4b333fcaf45b"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "2a0d903a5760900eaa7da02fa3a2c8d581933d9e10ce21030d2e28f9b7b8828c"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "cf7006e5c3e4b4c16c3a130211a7ed1b1fc5331659c01cc3d0e559c785428f34"
    end
  end

  def install
    bin.install "fh"
    bin.install "fhd"
  end

  service do
    run [opt_bin/"fhd", "--listen", "0.0.0.0:9876"]
    keep_alive true
    environment_variables FARHAND_TOKEN: "replace-with-your-token"
    log_path var/"log/fhd.log"
    error_log_path var/"log/fhd.err.log"
  end

  test do
    assert_match "Farhand client", shell_output("#{bin}/fh --help")
    assert_match "Farhand daemon", shell_output("#{bin}/fhd --help")
  end
end
