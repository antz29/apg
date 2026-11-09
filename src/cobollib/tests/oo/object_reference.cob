       IDENTIFICATION DIVISION.
       PROGRAM-ID. refdemo.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
           CLASS Account.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 acct OBJECT REFERENCE Account.
       01 any-obj OBJECT REFERENCE.
       01 holder.
           05 held OBJECT REFERENCE Account.
       77 cur-acct OBJECT REFERENCE Account.
       PROCEDURE DIVISION.
       MAIN-PARA.
           SET acct TO NULL.
           MOVE acct TO any-obj.
           STOP RUN.
