*** Settings ***
Suite Setup       Log    suite setup
Suite Teardown    Log    suite teardown
Test Tags         regression

*** Variables ***
${COUNT}          3

*** Test Cases ***
Passing Test
    [Tags]    smoke
    Log    starting    INFO
    Nested Keyword    2
    Should Be Equal    ${COUNT}    3

Failing Test
    Log    about to fail    WARN
    Level One
    Fail    expected failure

Skipped Test
    Skip    not now

Loop Test
    FOR    ${i}    IN RANGE    3
        Log    iteration ${i}
    END

Log With Html
    Log    <b>bold</b>    HTML
    Log    debug line    DEBUG

*** Keywords ***
Nested Keyword
    [Arguments]    ${depth}
    Log    depth ${depth}
    IF    ${depth} > 0
        ${next}=    Evaluate    ${depth} - 1
        Nested Keyword    ${next}
    END

Level One
    Level Two

Level Two
    Log    deep    INFO
