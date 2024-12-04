use alloy::sol;

sol!(
    #[sol(rpc)]
    contract ERC20 {
        function name() public view returns (string);
        function symbol() public view returns (string);
        function decimals() public view returns (uint8);
        function totalSupply() public view returns (uint256);
        function allowance(address _owner, address _spender) public view returns (uint256 remaining);
        function balanceOf(address _owner) public view returns (uint256 balance);
        function approve(address _spender, uint256 _value) public returns (bool success);
        function transfer(address _to, uint256 _value) public returns (bool success);

        event Transfer(address indexed from, address indexed to, uint256 value);
        event Approval(address indexed owner, address indexed spender, uint256 value);
    }

    #[sol(rpc)]
    contract PairV2 {
        function skim(address to) external;
    }

    #[sol(rpc)]
    interface WETH9 {
        event Approval(address indexed src, address indexed guy, uint wad);
        event Transfer(address indexed src, address indexed dst, uint wad);
        event Deposit(address indexed dst, uint wad);
        event Withdrawal(address indexed src, uint wad);
    }
);
