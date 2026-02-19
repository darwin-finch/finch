#!/usr/bin/env python3
"""
Migrate STATUS.md TODO items to GitHub Issues
"""
import re
import sys

def parse_status_md(path='STATUS.md'):
    """Parse STATUS.md and extract TODO items"""
    try:
        with open(path, 'r') as f:
            content = f.read()
    except FileNotFoundError:
        print(f"Error: {path} not found")
        sys.exit(1)
    
    issues = []
    current_phase = None
    
    for line in content.splitlines():
        # Detect phase headers
        if line.startswith('## Phase'):
            current_phase = line.strip('# ').strip()
            continue
        
        # Detect TODO items
        if line.strip().startswith('- [ ]'):
            title = line.strip('- [ ]').strip()
            issues.append({
                'title': title,
                'labels': [f'phase-{current_phase.lower().replace(" ", "-")}'] if current_phase else [],
                'body': f'From STATUS.md: {current_phase}' if current_phase else '',
            })
    
    return issues

def print_github_cli_commands(issues):
    """Print gh CLI commands to create issues"""
    print("# Run these commands to create GitHub issues:")
    print()
    
    for i, issue in enumerate(issues, 1):
        labels = ','.join(issue['labels'])
        print(f'gh issue create \\')
        print(f'  --title "{issue["title"]}" \\')
        print(f'  --body "{issue["body"]}" \\')
        print(f'  --label "{labels}"')
        print()

def main():
    issues = parse_status_md()
    print(f"Found {len(issues)} TODO items in STATUS.md")
    print()
    print_github_cli_commands(issues)
    print()
    print("# Or use this Python script with PyGithub:")
    print("#  pip install PyGithub")
    print("#  export GITHUB_TOKEN=your_token")
    print("#  python scripts/migrate_status_to_issues.py --create")

if __name__ == '__main__':
    if '--create' in sys.argv:
        print("Creating issues requires PyGithub. Install: pip install PyGithub")
        print("Then set GITHUB_TOKEN environment variable")
    else:
        main()
