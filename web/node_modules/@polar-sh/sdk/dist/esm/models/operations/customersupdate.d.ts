import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CustomerUpdate, CustomerUpdate$Outbound } from "../components/customerupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomersUpdateRequest = {
    /**
     * The customer ID.
     */
    id: string;
    customerUpdate: CustomerUpdate;
};
/** @internal */
export declare const CustomersUpdateRequest$inboundSchema: z.ZodType<CustomersUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersUpdateRequest$Outbound = {
    id: string;
    CustomerUpdate: CustomerUpdate$Outbound;
};
/** @internal */
export declare const CustomersUpdateRequest$outboundSchema: z.ZodType<CustomersUpdateRequest$Outbound, z.ZodTypeDef, CustomersUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersUpdateRequest$ {
    /** @deprecated use `CustomersUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersUpdateRequest$Outbound, z.ZodTypeDef, CustomersUpdateRequest>;
    /** @deprecated use `CustomersUpdateRequest$Outbound` instead. */
    type Outbound = CustomersUpdateRequest$Outbound;
}
export declare function customersUpdateRequestToJSON(customersUpdateRequest: CustomersUpdateRequest): string;
export declare function customersUpdateRequestFromJSON(jsonString: string): SafeParseResult<CustomersUpdateRequest, SDKValidationError>;
//# sourceMappingURL=customersupdate.d.ts.map