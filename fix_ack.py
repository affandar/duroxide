#!/usr/bin/env python3
"""Add vec![], (cancelled_sessions) as 8th arg to all .ack_orchestration_item() calls that only have 7 args."""

import os

files_to_check = [
    "src/provider_validation/atomicity.rs",
    "src/provider_validation/bulk_deletion.rs",
    "src/provider_validation/cancellation.rs",
    "src/provider_validation/capability_filtering.rs",
    "src/provider_validation/deletion.rs",
    "src/provider_validation/error_handling.rs",
    "src/provider_validation/instance_creation.rs",
    "src/provider_validation/instance_locking.rs",
    "src/provider_validation/lock_expiration.rs",
    "src/provider_validation/management.rs",
    "src/provider_validation/mod.rs",
    "src/provider_validation/multi_execution.rs",
    "src/provider_validation/poison_message.rs",
    "src/provider_validation/prune.rs",
    "src/provider_validation/queue_semantics.rs",
    "src/provider_validation/sessions.rs",
    "src/providers/sqlite.rs",
    "src/runtime/dispatchers/orchestration.rs",
    "tests/common/mod.rs",
    "tests/long_poll_tests.rs",
    "tests/cancellation_tests.rs",
    "tests/common/fault_injection.rs",
]

total_fixed = 0

for filepath in files_to_check:
    if not os.path.exists(filepath):
        print(f"SKIP (not found): {filepath}")
        continue

    with open(filepath, 'r') as f:
        lines = f.readlines()

    new_lines = []
    i = 0
    file_fixes = 0

    while i < len(lines):
        line = lines[i]

        if '.ack_orchestration_item(' in line and 'fn ack_orchestration_item' not in line:
            # Collect the full call from here to closing )
            call_start = i
            depth = 0
            j = i
            found_close = False
            close_line_idx = -1

            while j < len(lines):
                for ch in lines[j]:
                    if ch == '(':
                        depth += 1
                    elif ch == ')':
                        depth -= 1
                        if depth == 0:
                            close_line_idx = j
                            found_close = True
                            break
                if found_close:
                    break
                j += 1

            if not found_close:
                new_lines.append(line)
                i += 1
                continue

            # Count top-level commas inside the ack_orchestration_item( ... )
            full_call = ''.join(lines[i:close_line_idx + 1])
            paren_marker = '.ack_orchestration_item('
            paren_pos = full_call.index(paren_marker) + len(paren_marker)

            arg_depth = 0
            comma_count = 0
            last_non_ws = ''
            for ci in range(paren_pos, len(full_call)):
                ch = full_call[ci]
                if ch in '([{':
                    arg_depth += 1
                    last_non_ws = ch
                elif ch in ')]}':
                    if arg_depth == 0:
                        break  # closing paren of ack_orchestration_item
                    arg_depth -= 1
                    last_non_ws = ch
                elif ch == ',' and arg_depth == 0:
                    comma_count += 1
                    last_non_ws = ch
                elif ch not in ' \t\n\r':
                    last_non_ws = ch

            # If trailing comma, arg_count = comma_count; otherwise comma_count + 1
            if last_non_ws == ',':
                arg_count = comma_count
            else:
                arg_count = comma_count + 1

            if arg_count == 7:
                # Need to add 8th arg before closing )
                # Get indent from the previous non-empty arg line
                prev_line = lines[close_line_idx - 1]
                prev_indent = ''
                for ch in prev_line:
                    if ch in ' \t':
                        prev_indent += ch
                    else:
                        break

                # Add all lines up to (but not including) closing paren line
                for k in range(i, close_line_idx):
                    new_lines.append(lines[k])
                # Add new vec![], with same indent as previous arg
                new_lines.append(prev_indent + 'vec![],\n')
                # Add closing paren line
                new_lines.append(lines[close_line_idx])
                i = close_line_idx + 1
                file_fixes += 1
            elif arg_count == 8:
                # Already fixed, copy as-is
                for k in range(i, close_line_idx + 1):
                    new_lines.append(lines[k])
                i = close_line_idx + 1
            else:
                print(f"WARNING: {filepath}:{i+1} has {arg_count} args (expected 7 or 8)")
                new_lines.append(line)
                i += 1
        else:
            new_lines.append(line)
            i += 1

    if file_fixes > 0:
        with open(filepath, 'w') as f:
            f.writelines(new_lines)
        print(f"FIXED {file_fixes} calls in {filepath}")
        total_fixed += file_fixes
    else:
        print(f"OK (no changes needed): {filepath}")

print(f"\nTotal fixed: {total_fixed}")
