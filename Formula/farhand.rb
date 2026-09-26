class Farhand < Formula
  desc "Remote build and test offloader with zero external system binaries"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.8.1"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "df6f65697ef2379779f729da3a3b38f2372f8f69db770490f4b2e3f54b1f3e54"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "0adeeccaee8bc0e6926f8df6bbe3053f022e95f5103a4147188a57327fa9fa60"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "391211740920648d6dc123950d5e198dfda8fff6f6d7ac6e9b0adb537b1c72ff"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "38640a4652984863a7eb886f2a6a7f1065cddbd30bbeae66fa1da4eb9fd80e35"
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
