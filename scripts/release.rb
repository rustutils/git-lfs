#!/usr/bin/env ruby
# frozen_string_literal: true
#
# Cuts a GitLab release for a tag pipeline. Reads the matching
# CHANGELOG block, attaches every artifact under target/dist/ as a
# release asset link, and shells out to release-cli. Invoked from
# .gitlab-ci.yml's `release` job.
#
# Local dry-run (without actually pushing the release):
#   CI_COMMIT_TAG=v0.4.0 \
#   CI_PROJECT_URL=https://gitlab.com/rustutils/git-lfs \
#   ruby scripts/release.rb --print
#
# CHANGELOG lookup rules:
#   - Real release tag (e.g. v0.4.0): require a [0.4.0] block. Fail
#     otherwise — silently shipping a release with no notes is worse
#     than blocking it.
#   - Prerelease tag (e.g. v0.4.0-rc1, v0.4.0-test): try [0.4.0-rc1]
#     first, fall back to [Unreleased]. Prereleases normally do not
#     get their own dedicated CHANGELOG section.

require 'json'

CHANGELOG = 'CHANGELOG.md'
CARGO_TOML = 'Cargo.toml'
DIST_DIR  = 'target/dist'

print_only = ARGV.include?('--print')

tag         = ENV.fetch('CI_COMMIT_TAG')
project_url = ENV.fetch('CI_PROJECT_URL')

version    = tag.sub(/\Av/, '')
prerelease = version.include?('-')

# Read [workspace.package].version from Cargo.toml. Cheap regex
# walk — avoids pulling in a TOML gem for one field.
def cargo_version
  in_section = false
  File.foreach(CARGO_TOML) do |line|
    if line.start_with?('[')
      in_section = (line.strip == '[workspace.package]')
      next
    end
    if in_section && line =~ /\Aversion\s*=\s*"([^"]+)"/
      return Regexp.last_match(1)
    end
  end
  nil
end

# Bail if the tag doesn't match Cargo.toml exactly. We always bump
# Cargo.toml for crates.io anyway, so prereleases get the suffix in
# both places (Cargo.toml = "0.4.0-rc1", tag = v0.4.0-rc1).
crate_version = cargo_version
if crate_version.nil?
  warn "ERROR: no [workspace.package] version found in #{CARGO_TOML}"
  exit 1
end
unless crate_version == version
  warn "ERROR: tag #{tag} does not match Cargo.toml version #{crate_version}"
  exit 1
end

# Pull a `## [<heading>]` section out of CHANGELOG.md and return its
# body (everything until the next `## ` heading), trimmed.
def extract_section(heading)
  in_block = false
  buf = []
  File.foreach(CHANGELOG) do |line|
    if line.start_with?('## ')
      break if in_block
      in_block = line.match?(/\A## \[#{Regexp.escape(heading)}\]/)
      next
    end
    buf << line if in_block
  end
  buf.join.strip
end

notes = extract_section(version)
notes = extract_section('Unreleased') if notes.empty? && prerelease

if notes.empty?
  warn "ERROR: no CHANGELOG entry for [#{version}]"
  exit 1
end

# Asset URLs use the per-ref artifact endpoint, which resolves to the
# latest job artifact for that tag — stable as long as the package
# job artifacts do not expire.
asset_args = Dir["#{DIST_DIR}/*"].sort.flat_map do |path|
  name = File.basename(path)
  url  = "#{project_url}/-/jobs/artifacts/#{tag}/raw/#{path}?job=package"
  ['--assets-link', JSON.generate({ name: name, url: url })]
end

cmd = [
  'release-cli', 'create',
  '--name', "Release #{tag}",
  '--tag-name', tag,
  '--description', notes,
  *asset_args,
]

if print_only
  puts cmd.inspect
else
  exec(*cmd)
end
