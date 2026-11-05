# IBM and EXEC sample fixtures

Hand-written IBM dialect and `EXEC CICS` / `EXEC SQL` samples (fixed format, one
construct focus per file). No IBM source was available (SPEC O27), so every
expected fact below is **unverified** until an IBM source is found. Facts are
the SPEC §9 and §10 rules and the plan decisions for P4.

Dialect: EXEC files run under `ibm` (EXEC blocks recognised, OO recognition on).
Under `cobol85`, `cobol2002` and `cobol2014` every EXEC block is one error
diagnostic and is skipped, while the surrounding statements stay intact
(SPEC §6, §14, O26). `exec_callee.cob` is the in-scan target of the program
calls; it has no EXEC block and runs under any dialect.

| File | Construct exercised | Facts it should yield (SPEC §10) |
|---|---|---|
| `exec_callee.cob` | Plain program `EXECCALLEE`, target of the in-scan links and SQL CALL | Struct `EXECCALLEE` (File-contained); `MAIN-PARA` Function. No edges |
| `exec_link_literal.cob` | `EXEC CICS LINK PROGRAM('EXECCALLEE')` and `PROGRAM('OUTSIDEPGM')` | `uses` `EXECLINK.MAIN-PARA`→`EXECCALLEE` (in scan, as a literal `CALL`); `unresolved_call` `OUTSIDEPGM`, category `external` |
| `exec_xctl_variable.cob` | `EXEC CICS XCTL PROGRAM('EXECCALLEE')` (literal) and `PROGRAM(WS-PROG)` (variable) | literal → `uses` to `EXECCALLEE`; variable → `unresolved_call` category `func-value`. `WS-PROG` as data-reference use is not stated by the SPEC (see NOTE 2) |
| `exec_service.cob` | `EXEC CICS INVOKE SERVICE('SVCNAME')` and `INVOKE WEBSERVICE('WSNAME')` | Two `unresolved_call` category `external`, from `EXECSVC.MAIN-PARA`; no `uses` |
| `exec_transid.cob` | `EXEC CICS START TRANSID('TRN1')` and `RETURN TRANSID('TRN2')` | No program edge and no fact (O4); no unresolved target |
| `exec_sql_hostvars.cob` | `SELECT SALARY INTO :WS-SALARY:WS-SAL-IND FROM EMPLOYEE WHERE EMPNO = :WS-EMPNO` | `uses` `EXECHV.MAIN-PARA`→ 01 Structs `EXECHV.WS-SALARY` and `EXECHV.WS-EMPNO` (host variables are data references, SPEC §10). `:WS-SAL-IND` (indicator) gives **no** use: only the host variable `WS-SALARY` counts (plan decision). `EMPLOYEE` table → nothing |
| `exec_sql_call.cob` | `EXEC SQL CALL EXECCALLEE` and `EXEC SQL CALL SQLOUTSIDE` | `uses` `EXECSQLC.MAIN-PARA`→`EXECCALLEE` (in scan); `unresolved_call` `SQLOUTSIDE`, category `external` |
| `exec_sql_function.cob` | `SELECT COUNT(*), UPPER(LASTNAME) INTO :WS-CNT, :WS-UP FROM EMPLOYEE` | `uses` to 01 `WS-CNT` and `WS-UP` (host variables). `COUNT`, `UPPER`, `LASTNAME`, `EMPLOYEE` → nothing (SQL function and table name stay unlinked) |
| `exec_nested_if.cob` | `EXEC CICS LINK` inside `IF`, `EXEC CICS XCTL` inside `ELSE` | `uses` `EXECNEST.MAIN-PARA`→`EXECCALLEE` (LINK, in scan); `unresolved_call` `OUTSIDEPGM` category `external` (XCTL); the `IF` and `ELSE` statements stay intact |
| `exec_after_move.cob` | `MOVE 'A' TO WS-CODE` followed by `EXEC CICS RETURN TRANSID` in the same sentence (no period between) | `uses` `EXECMOVE.MAIN-PARA`→ 01 `WS-CODE` (from the MOVE); no transaction fact; `STOP RUN` after the period still scanned |
| `exec_data_division.cob` | DATA DIVISION `EXEC SQL INCLUDE SQLCA END-EXEC.`, `EXEC SQL BEGIN DECLARE SECTION END-EXEC.` … `EXEC SQL END DECLARE SECTION END-EXEC.` around 01 `HV-NAME` and `HV-ID` | Under `ibm`: the EXEC blocks are skipped with no fact and no diagnostic; 01 Structs `EXECDATA.HV-NAME` and `EXECDATA.HV-ID` still present. Under other dialects: an error diagnostic per EXEC block, the 01 entries still present |
| `ibm_oo_data_names.cob` | `OBJECT`, `METHOD`, `INVOKE` as 01 data names used in `MOVE`, `DISPLAY` and `IF` | Statements intact; 01 Structs `IBMOODATA.OBJECT`, `IBMOODATA.METHOD`, `IBMOODATA.INVOKE`; `MAIN-PARA` `uses` them. No diagnostic (NOTE 1) |
| `exec_non_ibm_error.cob` | `EXEC CICS LINK PROGRAM('EXECCALLEE')` between a `MOVE` and a `PERFORM` | Under `ibm`: `uses` to `EXECCALLEE`. Under `cobol85`, `cobol2002` and `cobol2014`: one error diagnostic for the EXEC block, block skipped (no `uses`); `MOVE`, `PERFORM OTHER-PARA` (`calls`) and `STOP RUN` stay intact |

## NOTE lines (for the coordinator)

- NOTE 1 (decided, `cobol-ibm-reserved-words`): under `ibm` the OO words are contextual keywords and `ibm` reserves no OO word (`Dialect::is_reserved` reserves only `ENTRY` and `ALTER` under `ibm`). So in `ibm_oo_data_names.cob`, `OBJECT`, `METHOD` and `INVOKE` parse as ordinary 01 data names, their `MOVE`, `DISPLAY` and `IF` statements stay intact, and no diagnostic is emitted. The exec e2e `ibm_oo_data_names_have_no_error_and_their_statements_give_uses` scans under `-std=ibm` and asserts: no stderr line names `ibm_oo_data_names.cob`; 01 Structs `OBJECT`, `METHOD` and `INVOKE` under `IBMOODATA`, located in that file; and a `uses` edge from `IBMOODATA.MAIN-PARA` to each.
- NOTE 2: the SPEC does not say whether the `PROGRAM(WS-PROG)` operand of a variable `XCTL` is also a data-reference `uses` of `WS-PROG`. The fixture does not decide; the expected-facts column records only the `func-value` call.
- NOTE 3: the task text writes `END DECLARE SECTION`; the fixture uses the full `EXEC SQL END DECLARE SECTION END-EXEC.` form, so the block is recognised as an EXEC SQL block.
- NOTE 4: the EXEC rules (§9, §10) and the indicator-variable rule are unverified until an IBM source is found (O27). `exec_sql_hostvars.cob` only tests the plan decision that the indicator is not a data reference.
