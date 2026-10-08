"""Shared test setup.

pouet.py answers out of a pouetdatadump-prods-*.json when one is lying beside
it or in the working directory -- which, running the tests from a checkout that
has one, is exactly what happens.  The API tests are about the API, so the dump
is off by default here; the dump's own tests turn it back on with a small
fixture file of their own."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import pouet


@pytest.fixture(autouse=True)
def no_pouet_dump(monkeypatch):
    """No data dump, and no dump loaded over from another test."""
    monkeypatch.setenv(pouet.DUMP_ENV, "")
    monkeypatch.setattr(pouet, "_dump", None)
    monkeypatch.setattr(pouet, "_dump_loaded", False)
