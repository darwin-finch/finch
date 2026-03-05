import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPaymentMethodCreate = {
    confirmationTokenId: string;
    setDefault: boolean;
    returnUrl: string;
};
/** @internal */
export declare const CustomerPaymentMethodCreate$inboundSchema: z.ZodType<CustomerPaymentMethodCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPaymentMethodCreate$Outbound = {
    confirmation_token_id: string;
    set_default: boolean;
    return_url: string;
};
/** @internal */
export declare const CustomerPaymentMethodCreate$outboundSchema: z.ZodType<CustomerPaymentMethodCreate$Outbound, z.ZodTypeDef, CustomerPaymentMethodCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPaymentMethodCreate$ {
    /** @deprecated use `CustomerPaymentMethodCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPaymentMethodCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPaymentMethodCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPaymentMethodCreate$Outbound, z.ZodTypeDef, CustomerPaymentMethodCreate>;
    /** @deprecated use `CustomerPaymentMethodCreate$Outbound` instead. */
    type Outbound = CustomerPaymentMethodCreate$Outbound;
}
export declare function customerPaymentMethodCreateToJSON(customerPaymentMethodCreate: CustomerPaymentMethodCreate): string;
export declare function customerPaymentMethodCreateFromJSON(jsonString: string): SafeParseResult<CustomerPaymentMethodCreate, SDKValidationError>;
//# sourceMappingURL=customerpaymentmethodcreate.d.ts.map