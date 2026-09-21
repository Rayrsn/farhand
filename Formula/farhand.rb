class Farhand < Formula
  desc "Zero-dependency remote build and test offloader for weak machines"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.2.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "266dde4d722210c41176053c84bd31e7c8f99245ff5ddebbbbf8d508f3d6c583"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "f3939cd7a422316461bb137918e4a2e355de44f13103317272911367b25a01cf"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "f602469b5172f40d7cad8e31ba933bca685a6ec6886679c79bed6871ff60c1da"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d7e0bc2b3547ac6916bd374a3ed9a4aa50414efd3ed729f7305d353d4160e242"
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
