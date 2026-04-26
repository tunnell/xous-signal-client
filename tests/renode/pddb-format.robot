# Adapted from antmicro/renode-xous-precursor (Apache-2.0):
# https://github.com/antmicro/renode-xous-precursor
#
# The original xous.robot establishes the pattern for driving Renode-emulated
# Xous via Robot Framework (machine setup, terminal testers on the SoC and EC
# UARTs, line-matching keywords). This file extends that pattern with a
# sigchat-specific PDDB format ceremony.
#
# Key project-specific finding (not in the upstream pattern): Xous's GAM
# radiobutton modal does not submit on Enter alone. The modal has items at
# indices 0..items.len()-1 plus an explicit "OK button" row at index
# items.len(). Enter at an item row only selects that item; Enter at the OK
# button row submits and closes. With the format dialog defaulting to cursor
# index 0 (Okay), the active selection must be navigated to the OK button row
# (Arrow Down x2) before Enter submits. Without this, the dialog appears to
# hang and the test times out at the llio time-offset spam.
#
# Successful run produces a formatted flash image that subsequent test runs
# can reuse to skip the ~15-minute format ceremony. Path is parameterised
# below; default points at xous-core/tools/pddb-images/renode.bin (the live
# Renode backing) and copies to /tmp on success.

*** Settings ***
Suite Setup       Setup
Suite Teardown    Teardown
Test Setup        Reset Emulation
Test Teardown     Test Teardown
Resource          ${RENODEKEYWORDS}

*** Variables ***
${SCRIPT}         ${CURDIR}/xous.resc
${CONSOLE}        sysbus.console
${EC_UART}        sysbus.uart
${FLASH_BACKING}  ${CURDIR}/xous-core/tools/pddb-images/renode.bin
${SAVED_FLASH}    /tmp/renode-pddb-formatted.bin

*** Keywords ***
Create Xous Machine
    Execute Script            ${SCRIPT}
    Create Terminal Tester    ${CONSOLE}    machine=SoC
    Create Terminal Tester    ${EC_UART}    machine=EC
    Execute Command           mach set "SoC"

Arrow Down
    Execute Command           keyboard InjectKey 0x1b
    Execute Command           keyboard InjectKey 0x5b
    Execute Command           keyboard InjectKey 0x42

*** Test Cases ***
Format PDDB And Save Flash
    [Documentation]    Boots blank flash, drives the PDDB format ceremony via keyboard
    ...                injection, saves the resulting formatted flash image.
    ...
    ...                Radiobutton mechanics: the modal has items [Okay, Cancel] plus
    ...                an explicit "OK button" row at index items.len(). Enter at an
    ...                item row only selects that item; Enter at the OK button row
    ...                submits and closes. Default cursor is at index 0 (Okay).
    ...                Navigate: Arrow Down x2 reaches the OK button, then Enter closes.
    ...
    ...                Ceremony sequence (SoC console, testerId=0):
    ...                  PDDB.REQFMT    -> Arrow Down x2 + Enter (confirm format via OK button)
    ...                  ROOTKEY.BOOTPW -> InjectLine "a" (boot PIN, bcrypt runs)
    ...                  PDDB.CHECKPASS -> InjectLine "" (dismiss re-enter notification)
    ...                  ROOTKEY.BOOTPW -> InjectLine "a" (boot PIN re-entry, bcrypt 2nd time)
    ...                  PDDB.MOUNTED   -> ceremony complete
    [Timeout]          60 minutes

    Create Xous Machine
    Start Emulation

    # Boot completes; PDDB reads blank flash (version=0xFFFFFFFF) -> Uninit -> try_mount_or_format
    # Radiobutton shows [Okay*, Cancel, OK button]. Cursor starts at 0 (Okay).
    # action_payload already defaults to Okay (first item added). Navigate to OK button then Enter.
    Wait For Line On Uart     PDDB.REQFMT    timeout=300    testerId=0
    Arrow Down
    Arrow Down
    Execute Command           keyboard InjectLine ""

    # rootkeys shows boot PIN TextEntry modal; bcrypt runs after submission
    Wait For Line On Uart     ROOTKEY.BOOTPW    timeout=300    testerId=0
    Execute Command           keyboard InjectLine "a"

    # After first bcrypt: pw_check logs PDDB.CHECKPASS then shows a blocking notification
    # ("Please re-enter your password to verify"). Any key dismisses it.
    Wait For Line On Uart     PDDB.CHECKPASS    timeout=1800    testerId=0
    Execute Command           keyboard InjectLine ""

    # clear_password() -> boot PIN modal again (pcache invalidated); second bcrypt runs
    Wait For Line On Uart     ROOTKEY.BOOTPW    timeout=300    testerId=0
    Execute Command           keyboard InjectLine "a"

    # Second bcrypt + PDDB erase + write + mount
    Wait For Line On Uart     PDDB.MOUNTED    timeout=1800    testerId=0

    # The flash backing file is live - copy for persistent reuse
    Run Process    cp    ${FLASH_BACKING}    ${SAVED_FLASH}
    Log    Formatted flash saved to ${SAVED_FLASH}

Reuse Saved Flash
    [Documentation]    Boots Renode with the flash formatted by the previous test and
    ...                verifies PDDB mounts without a format request.
    ...
    ...                Boot flow: try_login reads formatted flash -> calls unwrap_key ->
    ...                ensure_aes_password -> one ROOTKEY.BOOTPW modal -> bcrypt -> PDDB.MOUNTED
    [Timeout]          30 minutes

    Run Process    cp    ${SAVED_FLASH}    ${FLASH_BACKING}
    Create Xous Machine
    Start Emulation

    # try_login reads formatted flash; needs boot PIN to unwrap the system basis key
    Wait For Line On Uart     ROOTKEY.BOOTPW    timeout=300    testerId=0
    Execute Command           keyboard InjectLine "a"

    # One bcrypt pass then mount - no format request should appear
    Wait For Line On Uart     PDDB.MOUNTED    timeout=1800    testerId=0
    Log    PDDB mounted without format request - reuse confirmed
