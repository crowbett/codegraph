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
  version "0.1.2"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-macos-arm64.tar.gz"
      sha256 "1c07870b4462206d48dd4c0c1164aa826ad6d3ec3db646370827a94ea2381b68"
    end
    on_intel do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-macos-amd64.tar.gz"
      sha256 "0c7b0cf7f82679e403defaa003fddf9c20281261e6f7e223f02442e71541bb3c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-linux-arm64.tar.gz"
      sha256 "e2f9db9c05b5c7a777be04dcb6be4d558bad17484e169bd417ce16cb0d50e1bf"
    end
    on_intel do
      url "https://github.com/crowbett/codegraph/releases/download/v#{version}/codegraph-#{version}-linux-amd64.tar.gz"
      sha256 "42b25ebfcd5055dee1ba95313fbc97a4be2716c3d243e05122ba5fdd5ee2dd55"
    end
  end

  def install
    bin.install "codegraph"
  end

  test do
    assert_match "codegraph #{version}", shell_output("#{bin}/codegraph version")
  end
end
