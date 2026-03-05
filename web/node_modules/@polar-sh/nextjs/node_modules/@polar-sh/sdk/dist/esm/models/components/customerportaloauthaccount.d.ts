import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalOAuthAccount = {
    accountId: string;
    accountUsername: string | null;
};
/** @internal */
export declare const CustomerPortalOAuthAccount$inboundSchema: z.ZodType<CustomerPortalOAuthAccount, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalOAuthAccount$Outbound = {
    account_id: string;
    account_username: string | null;
};
/** @internal */
export declare const CustomerPortalOAuthAccount$outboundSchema: z.ZodType<CustomerPortalOAuthAccount$Outbound, z.ZodTypeDef, CustomerPortalOAuthAccount>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalOAuthAccount$ {
    /** @deprecated use `CustomerPortalOAuthAccount$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalOAuthAccount, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalOAuthAccount$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalOAuthAccount$Outbound, z.ZodTypeDef, CustomerPortalOAuthAccount>;
    /** @deprecated use `CustomerPortalOAuthAccount$Outbound` instead. */
    type Outbound = CustomerPortalOAuthAccount$Outbound;
}
export declare function customerPortalOAuthAccountToJSON(customerPortalOAuthAccount: CustomerPortalOAuthAccount): string;
export declare function customerPortalOAuthAccountFromJSON(jsonString: string): SafeParseResult<CustomerPortalOAuthAccount, SDKValidationError>;
//# sourceMappingURL=customerportaloauthaccount.d.ts.map