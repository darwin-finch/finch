import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Order's invoice data.
 */
export type CustomerOrderInvoice = {
    /**
     * The URL to the invoice.
     */
    url: string;
};
/** @internal */
export declare const CustomerOrderInvoice$inboundSchema: z.ZodType<CustomerOrderInvoice, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerOrderInvoice$Outbound = {
    url: string;
};
/** @internal */
export declare const CustomerOrderInvoice$outboundSchema: z.ZodType<CustomerOrderInvoice$Outbound, z.ZodTypeDef, CustomerOrderInvoice>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerOrderInvoice$ {
    /** @deprecated use `CustomerOrderInvoice$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerOrderInvoice, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerOrderInvoice$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerOrderInvoice$Outbound, z.ZodTypeDef, CustomerOrderInvoice>;
    /** @deprecated use `CustomerOrderInvoice$Outbound` instead. */
    type Outbound = CustomerOrderInvoice$Outbound;
}
export declare function customerOrderInvoiceToJSON(customerOrderInvoice: CustomerOrderInvoice): string;
export declare function customerOrderInvoiceFromJSON(jsonString: string): SafeParseResult<CustomerOrderInvoice, SDKValidationError>;
//# sourceMappingURL=customerorderinvoice.d.ts.map