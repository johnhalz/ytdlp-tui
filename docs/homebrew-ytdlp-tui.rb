# frozen_string_literal: true

# Template for a separate tap repo, e.g. github.com/johnhalz/homebrew-ytdlp-tui
# Replace version and each sha256 after cutting a release.
class YtdlpTui < Formula
  desc "Interactive terminal UI for yt-dlp"
  homepage "https://github.com/johnhalz/ytdlp-tui"
  version "0.1.1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/johnhalz/ytdlp-tui/releases/download/v#{version}/ytdlp-tui-macos-arm64"
      sha256 "REPLACE_SHA256_MACOS_ARM64"
    else
      odie "ytdlp-tui: Intel Mac binaries are not built in CI; install with pip, uv, or PyPI."
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      odie "ytdlp-tui: Linux arm64 is not built in CI yet; install via pip/uv or extend release.yml."
    end
    url "https://github.com/johnhalz/ytdlp-tui/releases/download/v#{version}/ytdlp-tui-linux-x86_64"
    sha256 "REPLACE_SHA256_LINUX_X86_64"
  end

  def install
    binary = if OS.mac?
      "ytdlp-tui-macos-arm64"
    else
      "ytdlp-tui-linux-x86_64"
    end
    bin.install binary => "ytdlp-tui"
  end

  test do
    system "#{bin}/ytdlp-tui", "--help"
  end
end
