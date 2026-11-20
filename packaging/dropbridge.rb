# Homebrew formula: the source of truth for Formula/dropbridge.rb in github.com/Aayushkalikote/homebrew-tap.
# release.yml fills in url and sha256 for each release and pushes it there; edit it here, not in the tap.
class Dropbridge < Formula
  desc "Show Android phones in macOS AirDrop and receive Quick Share on your Mac"
  homepage "https://github.com/Aayushkalikote/DropBridge"
  # Prebuilt universal binary from the GitHub release, so users need no Rust toolchain.
  url "https://github.com/Aayushkalikote/DropBridge/releases/download/v0.1.1/dropbridge-v0.1.1-macos-universal.tar.gz"
  sha256 "cffb6c33c2ff20e354ddfb8384760a51f34bb9e9d184f728ab987a28edb211f3"
  license "GPL-3.0-only"

  head do
    url "https://github.com/Aayushkalikote/DropBridge.git", branch: "main"
    depends_on "protobuf" => :build # rqs_lib generates its protocol code with protoc
    depends_on "rust" => :build
  end

  depends_on :macos

  def install
    if build.head?
      system "cargo", "install", *std_cargo_args
    else
      bin.install "dropbridge"
    end
    # What's new, printed in the caveats: this version's changelog section from the bundled README
    news = File.read("README.md")[%r{^### v#{Regexp.escape(version.to_s)}\n(.*?)(?=^###|^</details>)}m, 1]
    (pkgshare/"changes.txt").write(news.strip) if news && !news.strip.empty?
  end

  service do
    run [opt_bin/"dropbridge"]
    keep_alive true
    process_type :interactive
    log_path var/"log/dropbridge.log"
    error_log_path var/"log/dropbridge.log"
  end

  def caveats
    changes = pkgshare/"changes.txt"
    news = changes.exist? ? "🎉 What's new in DropBridge #{version}:\n#{changes.read.gsub(/^/, "  ")}\n\n" : ""
    news + <<~EOS
      Start it now and at every login:
        brew services start dropbridge

      After an upgrade, load the new version:
        brew services restart dropbridge

      On the phone: same Wi-Fi, Quick Share → Everyone. Then check with:
        dropbridge doctor
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dropbridge --version")
  end
end
