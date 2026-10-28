# Homebrew formula. Lives in github.com/Aayushkalikote/homebrew-tap as Formula/dropbridge.rb.
class Dropbridge < Formula
  desc "Show Android phones in macOS AirDrop and receive Quick Share on your Mac"
  homepage "https://github.com/Aayushkalikote/DropBridge"
  url "https://github.com/Aayushkalikote/DropBridge/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "8682142f3df7e7058f9d959936fb945e8c7d7e2f5712c09511b0e7fda89d608d"
  license "GPL-3.0-only"
  head "https://github.com/Aayushkalikote/DropBridge.git", branch: "main"

  depends_on "protobuf" => :build # rqs_lib generates its protocol code with protoc
  depends_on "rust" => :build
  depends_on :macos

  def install
    system "cargo", "install", *std_cargo_args
    # What's new, printed in the caveats after install and upgrade; empty for HEAD builds
    notes = Utils.popen_read("bash", ".github/release-notes.sh", "v#{version}")
    (pkgshare/"changes.txt").write(notes) unless notes.empty?
  end

  service do
    run [opt_bin/"dropbridge"]
    keep_alive true
    process_type :interactive
    log_path var/"log/dropbridge.log"
    error_log_path var/"log/dropbridge.log"
  end

  def caveats
    notes = pkgshare/"changes.txt"
    news = if notes.exist?
      items = notes.read.gsub(/^- \*\*(\w+):\*\*/, '  - \1:')
      "🎉 What's new in DropBridge #{version}:\n#{items}\n"
    else
      ""
    end
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
