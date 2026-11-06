       IDENTIFICATION DIVISION.
       CLASS-ID. Ledger.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
       OBJECT.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       PROCEDURE DIVISION.
       METHOD-ID. first-entry.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
           COPY acctrec.
       PROCEDURE DIVISION.
           MOVE 1 TO acct-no.
           DISPLAY acct-name.
       END METHOD first-entry.
       END OBJECT.
       END CLASS Ledger.
