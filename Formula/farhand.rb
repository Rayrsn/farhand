class Farhand < Formula
  desc "Remote build and test offloader with zero external system binaries"
  homepage "https://github.com/Rayrsn/farhand"
  version "1.9.0"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-apple-darwin.tar.gz"
      sha256 "16fd9f47636d82d467f92877c80daea63044dd632e0908b1445851a5eb9981db"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-apple-darwin.tar.gz"
      sha256 "9fbb7b9c3d5ab87eae768dbe61d09532243fd7907c393a1752bd8e79ba5735ce"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-aarch64-unknown-linux-musl.tar.gz"
      sha256 "f6de2fd1c9976b3c8d779949688393ebf01554cac209a9b39351e0ed3d04bcdc"
    else
      url "https://github.com/Rayrsn/farhand/releases/download/v#{version}/farhand-x86_64-unknown-linux-musl.tar.gz"
      sha256 "73e82d954c70b45f035e4637b64578d340e8048b81dd0585069b9a10bf317fd6"
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
