# The Python toolchain for the black-box LSP test layer (it/lsp-blackbox):
# pytest-lsp drives the REAL ls-server binary over stdio like an editor would,
# and lsp-devtools (packaged here from PyPI — not in nixpkgs) records/inspects
# JSON-RPC traffic for interactive debugging (`lsp-devtools record`, TUI).
{ pkgs }:

let
  python = pkgs.python3;

  lsp-devtools = python.pkgs.buildPythonPackage rec {
    pname = "lsp-devtools";
    version = "0.4.0";
    pyproject = true;
    src = pkgs.fetchPypi {
      pname = "lsp_devtools";
      inherit version;
      sha256 = "9fdb21b50f9ff0c0cbcdcf8c369195cc08e70dbccf2321db62c60c46b63fb38b";
    };
    build-system = [ python.pkgs.hatchling ];
    dependencies = with python.pkgs; [
      aiosqlite
      platformdirs
      pygls
      stamina
      textual
    ];
    # The sdist carries no tests; the import check proves the wiring.
    doCheck = false;
    pythonImportsCheck = [ "lsp_devtools" ];
  };

  pythonEnv = python.withPackages (ps: [
    ps.pytest
    ps.pytest-asyncio
    ps.pytest-lsp
    lsp-devtools
  ]);
in
{
  inherit pythonEnv lsp-devtools;
}
