       IDENTIFICATION DIVISION.
       CLASS-ID. Savings INHERITS Account IMPLEMENTS Printable.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
           CLASS Account.
           INTERFACE Printable.
       OBJECT.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 fee PIC 9V99 VALUE 0.
       PROCEDURE DIVISION.
       METHOD-ID. render.
       DATA DIVISION.
       LINKAGE SECTION.
       01 out-line PIC X(80).
       PROCEDURE DIVISION RETURNING out-line.
           INVOKE SELF "deposit" USING fee.
           MOVE "savings" TO out-line.
       END METHOD render.
       METHOD-ID. deposit.
       DATA DIVISION.
       LINKAGE SECTION.
       01 amount PIC S9(7)V99.
       PROCEDURE DIVISION USING amount.
           INVOKE SUPER "deposit" USING amount.
       END METHOD deposit.
       END OBJECT.
       END CLASS Savings.
