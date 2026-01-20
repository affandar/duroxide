#!/usr/bin/env python3
"""Convert legacy Duroxide API patterns to simplified patterns."""

import re
import sys
import glob

def convert_content(content):
    """Convert legacy API patterns to simplified patterns."""
    
    # Pattern 1: schedule_activity("name", arg).into_activity().await
    # -> simplified_schedule_activity("name", arg).await
    # Use a more robust pattern that handles nested parens
    content = re.sub(
        r'\.schedule_activity\((.+?)\)\.into_activity\(\)\.await',
        r'.simplified_schedule_activity(\1).await',
        content
    )
    
    # Pattern 2: schedule_timer(duration).into_timer().await
    # -> simplified_schedule_timer(duration).await
    # Handle Duration::from_millis(x) which has nested parens
    content = re.sub(
        r'\.schedule_timer\((Duration::[^)]+\([^)]+\))\)\.into_timer\(\)\.await',
        r'.simplified_schedule_timer(\1).await',
        content
    )
    
    # Fallback for simpler schedule_timer patterns
    content = re.sub(
        r'\.schedule_timer\(([^)]+)\)\.into_timer\(\)\.await',
        r'.simplified_schedule_timer(\1).await',
        content
    )
    
    # Pattern 3: schedule_wait("event").into_event().await
    # -> simplified_schedule_wait("event").await
    content = re.sub(
        r'\.schedule_wait\(([^)]+)\)\.into_event\(\)\.await',
        r'.simplified_schedule_wait(\1).await',
        content
    )
    
    # Pattern 4: schedule_wait_typed::<T>("event").into_event_typed::<T>().await
    # -> simplified_schedule_wait_typed::<T>("event").await
    content = re.sub(
        r'\.schedule_wait_typed::<([^>]+)>\(([^)]+)\)\.into_event_typed::<[^>]+>\(\)\.await',
        r'.simplified_schedule_wait_typed::<\1>(\2).await',
        content
    )
    
    # Pattern 5: schedule_sub_orchestration("name", input).into_sub_orchestration().await
    # -> simplified_schedule_sub_orchestration("name", input).await
    content = re.sub(
        r'\.schedule_sub_orchestration\(([^)]+)\)\.into_sub_orchestration\(\)\.await',
        r'.simplified_schedule_sub_orchestration(\1).await',
        content
    )
    
    return content

def convert_file(filepath):
    """Convert a single file."""
    with open(filepath, 'r') as f:
        content = f.read()
    
    new_content = convert_content(content)
    
    if content != new_content:
        with open(filepath, 'w') as f:
            f.write(new_content)
        print(f"Converted: {filepath}")
    else:
        print(f"No changes: {filepath}")

def main():
    if len(sys.argv) > 1:
        # Convert specific files
        for filepath in sys.argv[1:]:
            convert_file(filepath)
    else:
        # Convert all test files
        for filepath in glob.glob('tests/*.rs'):
            convert_file(filepath)

if __name__ == '__main__':
    main()
