       IDENTIFICATION DIVISION.
       PROGRAM-ID. invokes.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
           CLASS Account.
           CLASS Widget AS "com.example.Widget".
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 acct OBJECT REFERENCE Account.
       01 ext OBJECT REFERENCE Widget.
       01 any-obj OBJECT REFERENCE.
       01 amount PIC S9(7)V99 VALUE 5.
       01 out-bal PIC S9(7)V99.
       PROCEDURE DIVISION.
       main-para.
           INVOKE Account "create" RETURNING acct.
           INVOKE acct "deposit" USING amount.
           INVOKE acct "balance-of" RETURNING out-bal.
           INVOKE ext "poke".
           INVOKE any-obj "poke".
           STOP RUN.
