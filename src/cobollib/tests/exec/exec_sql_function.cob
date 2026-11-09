       IDENTIFICATION DIVISION.
       PROGRAM-ID. EXECSQLF.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 WS-CNT PIC S9(9) COMP.
       01 WS-UP PIC X(20).
       PROCEDURE DIVISION.
       MAIN-PARA.
           EXEC SQL
               SELECT COUNT(*), UPPER(LASTNAME)
                 INTO :WS-CNT, :WS-UP
                 FROM EMPLOYEE
           END-EXEC.
           STOP RUN.
