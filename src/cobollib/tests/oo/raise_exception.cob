       IDENTIFICATION DIVISION.
       CLASS-ID. Validator.
       ENVIRONMENT DIVISION.
       CONFIGURATION SECTION.
       REPOSITORY.
           CLASS Failure AS "Failure".
       OBJECT.
       DATA DIVISION.
       WORKING-STORAGE SECTION.
       01 err-obj OBJECT REFERENCE Failure.
       PROCEDURE DIVISION.
       METHOD-ID. check.
       DATA DIVISION.
       LINKAGE SECTION.
       01 value-in PIC 9(3).
       PROCEDURE DIVISION USING value-in.
           IF value-in > 100
               RAISE EXCEPTION EC-USER
           END-IF.
           IF value-in = 0
               RAISE err-obj
           END-IF.
       END METHOD check.
       END OBJECT.
       END CLASS Validator.
