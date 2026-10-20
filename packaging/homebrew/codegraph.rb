# Homebrew formula for codegraph (prepared, not published).
#
# Lives in a tap repo such as crowbett/homebrew-tap as Formula/codegraph.rb:
#   brew tap crowbett/tap
#   brew install codegraph
#
# The tap is not published yet; docs/INSTALL.md lists the install channels that work today.
#
# On each release: bump `version` and replace every sha256 with the matching line from
# the release's SHA256SUMS.
class Codegraph < Formula
  desc "Tree-sitter symbol + call graph of every repo you own, served to AI agents over MCP"
  homepage "https://github.com/crowbett/codegraph"
  version "0.1.1"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-macos-arm64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-macos-amd64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-linux-arm64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-linux-amd64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "codegraph"
  end

  test do
    assert_match "codegraph #{version}", shell_output("#{bin}/codegraph version")
  end
end
