       IDENTIFICATION DIVISION.
       CLASS-ID. Account.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
       FACTORY.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 account-count PIC 9(5) VALUE 0.
       PROCEDURE DIVISION.
       METHOD-ID. create.
       DATA DIVISION.
       LINKAGE SECTION.
       01 new-acct OBJECT REFERENCE Account.
       PROCEDURE DIVISION RETURNING new-acct.
           INVOKE SELF "new" RETURNING new-acct.
           ADD 1 TO account-count.
       END METHOD create.
       END FACTORY.
       OBJECT.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 balance PIC S9(7)V99 VALUE 0.
       PROCEDURE DIVISION.
       METHOD-ID. deposit.
       DATA DIVISION.
       LINKAGE SECTION.
       01 amount PIC S9(7)V99.
       PROCEDURE DIVISION USING amount.
           ADD amount TO balance.
       END METHOD deposit.
       METHOD-ID. balance-of.
       DATA DIVISION.
       LINKAGE SECTION.
       01 balance-out PIC S9(7)V99.
       PROCEDURE DIVISION RETURNING balance-out.
           MOVE balance TO balance-out.
       END METHOD balance-of.
       END OBJECT.
       END CLASS Account.
